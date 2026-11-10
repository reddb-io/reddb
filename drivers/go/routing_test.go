package reddb

import (
	"context"
	"errors"
	contract "github.com/reddb-io/reddb-go/internal/routing"
	"io"
	"strings"
	"testing"
	"time"
)

type routingPeer struct {
	Conn
	address    string
	membership func() *contract.Topology
	calls      *[][2]string
}

func (p *routingPeer) Query(_ context.Context, sql string, _ ...any) ([]byte, error) {
	*p.calls = append(*p.calls, [2]string{p.address, sql})
	if sql == "UPDATE lost" || strings.Contains(sql, "effectful") {
		return nil, io.EOF
	}
	return []byte(p.address), nil
}
func (p *routingPeer) Ping(context.Context) error { return nil }
func (p *routingPeer) Close() error               { return nil }
func (p *routingPeer) topology(context.Context, string) (*contract.Topology, error) {
	return p.membership(), nil
}
func TestRoutingAcrossProtocols(t *testing.T) {
	for _, protocol := range []string{"red", "reds", "grpc", "grpcs", "http", "https"} {
		t.Run(protocol, func(t *testing.T) {
			ctx := context.Background()
			primary := protocol + "://primary:5000"
			replica := protocol + "://replica:5000"
			active := primary
			calls := [][2]string{}
			membership := func() *contract.Topology {
				return &contract.Topology{Version: 1, Primary: contract.Endpoint{Addr: active},
					Replicas: []contract.Replica{{Endpoint: contract.Endpoint{Addr: replica}, Healthy: true, LagMS: 10}}}
			}
			dial := func(_ context.Context, address string) (Conn, error) {
				return &routingPeer{address: address, membership: membership, calls: &calls}, nil
			}
			initial, _ := dial(ctx, primary)
			router := &routedConn{seed: primary, primary: primary, protocol: protocol, clients: map[string]Conn{primary: initial}, dial: dial}
			if err := router.refresh(ctx); err != nil {
				t.Fatal(err)
			}
			assertTarget := func(want string, read bool) {
				t.Helper()
				var got []byte
				var err error
				if read {
					got, err = QueryWithOptions(ctx, router, "SELECT 1", ReadOptions{Consistency: ReadLocal})
				} else {
					got, err = router.Query(ctx, "SELECT 1")
				}
				if err != nil || string(got) != want {
					t.Fatalf("target=%s want=%s err=%v", got, want, err)
				}
			}
			assertTarget(primary, false)
			assertTarget(replica, true)
			if _, err := router.Query(ctx, "/* transaction */ BEGIN"); err != nil {
				t.Fatal(err)
			}
			active = protocol + "://promoted:5000"
			router.refreshAt = time.Time{}
			assertTarget(primary, true)
			if _, err := router.Query(ctx, "ROLLBACK TO SAVEPOINT nested"); err != nil {
				t.Fatal(err)
			}
			assertTarget(primary, true)
			if _, err := router.Query(ctx, "ROLLBACK"); err != nil {
				t.Fatal(err)
			}
			assertTarget(active, false)
			if _, err := router.Query(ctx, "UPDATE lost"); !errors.Is(err, io.EOF) {
				t.Fatalf("expected lost reply, got %v", err)
			}
			writes := 0
			for _, call := range calls {
				if call[1] == "UPDATE lost" {
					writes++
				}
			}
			if writes != 1 {
				t.Fatal("write replay")
			}
			before := len(calls)
			if _, err := QueryWithOptions(ctx, router, `SELECT "effectful"()`, ReadOptions{Consistency: ReadLocal}); !errors.Is(err, io.EOF) {
				t.Fatal(err)
			}
			if len(calls) != before+1 || calls[len(calls)-1][0] != active {
				t.Fatal("effectful quoted SQL replayed or sent to replica")
			}
			_ = router.Close()
			if _, err := router.Query(ctx, "SELECT 1"); err == nil {
				t.Fatal("query after close")
			}
		})
	}
}

func TestCancelDuringReplicaPreparation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	primary, replica := "http://primary:5000", "http://replica:5000"
	calls := [][2]string{}
	membership := func() *contract.Topology {
		return &contract.Topology{Version: 1, Primary: contract.Endpoint{Addr: primary}, Replicas: []contract.Replica{{Endpoint: contract.Endpoint{Addr: replica}, Healthy: true}}}
	}
	initial := &routingPeer{address: primary, membership: membership, calls: &calls}
	dial := func(ctx context.Context, address string) (Conn, error) {
		if address == replica {
			cancel()
			return nil, ctx.Err()
		}
		return &routingPeer{address: address, membership: membership, calls: &calls}, nil
	}
	router := &routedConn{seed: primary, primary: primary, protocol: "http", clients: map[string]Conn{primary: initial}, dial: dial}
	if err := router.refresh(ctx); !errors.Is(err, context.Canceled) {
		t.Fatalf("expected cancellation, got %v", err)
	}
	if len(router.replicas) != 0 {
		t.Fatal("published canceled membership")
	}
}

func TestConnectRejectsAlreadyCanceledContext(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	client, err := Connect(ctx, "http://127.0.0.1:5000")
	if client != nil {
		_ = client.Close()
	}
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("expected cancellation, got %v", err)
	}
}

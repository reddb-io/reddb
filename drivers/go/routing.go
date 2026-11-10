package reddb

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/reddb-io/reddb-go/httpx"
	contract "github.com/reddb-io/reddb-go/internal/routing"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

type ReadConsistency string

const (
	ReadStrong           ReadConsistency = "strong"
	ReadLocal            ReadConsistency = "local"
	ReadBoundedStaleness ReadConsistency = "bounded_staleness"
)

type ReadOptions struct {
	Consistency ReadConsistency
	MaxLag      time.Duration
}
type ReadConn interface {
	QueryWithOptions(context.Context, string, ReadOptions, ...any) ([]byte, error)
}

// QueryWithOptions explicitly opts a SQL read into replica routing.
func QueryWithOptions(ctx context.Context, client Conn, sql string, options ReadOptions, params ...any) ([]byte, error) {
	if reader, ok := client.(ReadConn); ok {
		return reader.QueryWithOptions(ctx, sql, options, params...)
	}
	return nil, NewError("CONSISTENCY_UNSUPPORTED", "this connection cannot enforce read consistency")
}

type topologySource interface {
	topology(context.Context, string) (*contract.Topology, error)
}
type routedConn struct {
	mu                                    sync.Mutex
	seed, primary, protocol, pinned       string
	session, broken, closed, forcePrimary bool
	clients                               map[string]Conn
	replicas                              []contract.Replica
	next                                  uint64
	refreshAt                             time.Time
	dial                                  func(context.Context, string) (Conn, error)
}

func connectRouted(ctx context.Context, p *ParsedURI, o *Options) (Conn, error) {
	protocol := map[Kind]string{KindRedWire: "red", KindRedWires: "reds", KindHTTP: "http", KindHTTPS: "https", KindGRPC: "grpc", KindGRPCS: "grpcs"}[p.Kind]
	seed := contract.Address(protocol+"://"+net.JoinHostPort(p.Host, strconv.Itoa(p.Port))+p.Path, protocol)
	initial, err := connectDirect(ctx, p, o)
	if err != nil {
		return nil, err
	}
	router := &routedConn{seed: seed, primary: seed, protocol: protocol, clients: map[string]Conn{seed: initial},
		forcePrimary: strings.EqualFold(p.Params.Get("route"), "primary")}
	router.dial = func(ctx context.Context, address string) (Conn, error) {
		target, err := ParseURI(address)
		if err != nil {
			return nil, err
		}
		clone := *p
		clone.Host = target.Host
		clone.Port = target.Port
		clone.Path = target.Path
		return connectDirect(ctx, &clone, o)
	}
	if err := router.refresh(ctx); err != nil {
		_ = router.Close()
		return nil, err
	}
	return router, nil
}
func (r *routedConn) endpoint(ctx context.Context, address string) (Conn, error) {
	if client, ok := r.clients[address]; ok {
		return client, nil
	}
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	client, err := r.dial(bounded, address)
	if err != nil {
		return nil, err
	}
	if err = client.Ping(bounded); err != nil {
		_ = client.Close()
		return nil, err
	}
	r.clients[address] = client
	return client, nil
}
func authError(err error) bool {
	var refusal *httpx.ResponseError
	if errors.As(err, &refusal) && (refusal.Status == 401 || refusal.Status == 403) {
		return true
	}
	var driver *Error
	return errors.As(err, &driver) && driver.Code == CodeAuthRefused || status.Code(err) == codes.Unauthenticated || status.Code(err) == codes.PermissionDenied
}
func networkError(err error) bool {
	var network net.Error
	var driver *Error
	return errors.As(err, &network) || errors.Is(err, io.EOF) || errors.Is(err, io.ErrUnexpectedEOF) ||
		errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) ||
		errors.As(err, &driver) && driver.Code == CodeNetwork ||
		status.Code(err) == codes.Unavailable || status.Code(err) == codes.DeadlineExceeded
}
func (r *routedConn) discover(ctx context.Context, address string) (*contract.Topology, error) {
	bounded, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	client, err := r.dial(bounded, address)
	if err != nil {
		return nil, err
	}
	defer client.Close()
	source, ok := client.(topologySource)
	if !ok {
		return nil, nil
	}
	return source.topology(bounded, r.protocol)
}
func (r *routedConn) refresh(ctx context.Context) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	if r.pinned != "" || r.closed {
		return nil
	}
	r.refreshAt = time.Now().Add(30 * time.Second)
	budget, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()
	candidates := []string{r.primary, r.seed}
	for _, replica := range r.replicas {
		candidates = append(candidates, replica.Addr)
	}
	for index := 0; index < len(candidates) && index < 258 && budget.Err() == nil; index++ {
		topology, err := r.discover(budget, candidates[index])
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			if authError(err) {
				return err
			}
			continue
		}
		if topology == nil || topology.Version != 1 {
			continue
		}
		primary := contract.Address(topology.Primary.Addr, r.protocol)
		if primary == "" {
			continue
		}
		present := false
		for _, candidate := range candidates {
			present = present || candidate == primary
		}
		if !present {
			candidates = append(candidates, primary)
		}
		if _, err = r.endpoint(budget, primary); err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			continue
		}
		replicas := []contract.Replica{}
		for index, replica := range topology.Replicas {
			if index >= 256 || budget.Err() != nil {
				break
			}
			replica.Addr = contract.Address(replica.Addr, r.protocol)
			if replica.Addr == "" || replica.Addr == primary || !replica.Healthy || replica.Rebootstrapping {
				continue
			}
			if _, err = r.endpoint(budget, replica.Addr); err == nil {
				replicas = append(replicas, replica)
			}
			if ctx.Err() != nil {
				return ctx.Err()
			}
		}
		if ctx.Err() != nil {
			return ctx.Err()
		}
		r.primary = primary
		r.replicas = replicas
		keep := map[string]bool{r.seed: true, primary: true}
		for _, replica := range replicas {
			keep[replica.Addr] = true
		}
		for address, client := range r.clients {
			if !keep[address] {
				_ = client.Close()
				delete(r.clients, address)
			}
		}
		if primary == candidates[index] {
			return nil
		}
	}
	return ctx.Err()
}
func invokeRouted[T any](r *routedConn, ctx context.Context, sql string, read bool, options *ReadOptions, operation func(Conn) (T, error)) (T, error) {
	r.mu.Lock()
	defer r.mu.Unlock()
	var zero T
	if r.closed || r.broken {
		return zero, NewError(CodeClosed, "connection is closed")
	}
	if r.pinned == "" && !time.Now().Before(r.refreshAt) {
		if err := r.refresh(ctx); err != nil {
			return zero, err
		}
	}
	if options != nil && (options.Consistency != ReadLocal && options.Consistency != ReadStrong && options.Consistency != ReadBoundedStaleness || options.MaxLag < 0) {
		return zero, NewError("CONSISTENCY_UNSUPPORTED", "unsupported read consistency")
	}
	state, safe := "primary", false
	if sql != "" {
		state, safe = contract.SQLPolicy(sql)
	}
	read = read || safe && options != nil
	eligible := []contract.Replica{}
	if !r.forcePrimary && (options == nil || options.Consistency != ReadStrong) {
		for _, replica := range r.replicas {
			if options == nil || options.Consistency != ReadBoundedStaleness || time.Duration(replica.LagMS)*time.Millisecond <= options.MaxLag {
				eligible = append(eligible, replica)
			}
		}
	}
	address := r.primary
	if r.pinned != "" {
		address = r.pinned
	} else if read && len(eligible) > 0 {
		address = eligible[r.next%uint64(len(eligible))].Addr
		r.next++
	}
	if state == "begin" || state == "session" {
		r.pinned = address
		r.session = r.session || state == "session"
	}
	client, err := r.endpoint(ctx, address)
	var result T
	if err == nil {
		result, err = operation(client)
	}
	if err == nil {
		if state == "end" && !r.session {
			r.pinned = ""
		}
		return result, nil
	}
	if !networkError(err) {
		return zero, err
	}
	r.refreshAt = time.Time{}
	if client := r.clients[address]; client != nil {
		_ = client.Close()
		delete(r.clients, address)
	}
	if r.pinned != "" {
		r.broken = true
	}
	if r.pinned != "" || !read || ctx.Err() != nil {
		return zero, err
	}
	if err = r.refresh(ctx); err != nil {
		return zero, err
	}
	client, err = r.endpoint(ctx, r.primary)
	if err != nil {
		return zero, err
	}
	return operation(client)
}
func (r *routedConn) Query(ctx context.Context, sql string, params ...any) ([]byte, error) {
	return invokeRouted(r, ctx, sql, false, nil, func(c Conn) ([]byte, error) { return c.Query(ctx, sql, params...) })
}
func (r *routedConn) QueryWithOptions(ctx context.Context, sql string, options ReadOptions, params ...any) ([]byte, error) {
	return invokeRouted(r, ctx, sql, false, &options, func(c Conn) ([]byte, error) { return c.Query(ctx, sql, params...) })
}
func (r *routedConn) Exec(ctx context.Context, sql string, params ...any) (Result, error) {
	body, err := r.Query(ctx, sql, params...)
	if err != nil {
		return Result{}, err
	}
	return execResultFromJSON(body)
}
func (r *routedConn) Insert(ctx context.Context, col string, payload any) error {
	_, err := invokeRouted(r, ctx, "", false, nil, func(c Conn) (bool, error) { return true, c.Insert(ctx, col, payload) })
	return err
}
func (r *routedConn) BulkInsert(ctx context.Context, col string, rows []any) (*BulkInsertResult, error) {
	return invokeRouted(r, ctx, "", false, nil, func(c Conn) (*BulkInsertResult, error) { return c.BulkInsert(ctx, col, rows) })
}
func (r *routedConn) Get(ctx context.Context, col, id string) ([]byte, error) {
	return invokeRouted(r, ctx, "", true, nil, func(c Conn) ([]byte, error) { return c.Get(ctx, col, id) })
}
func (r *routedConn) Delete(ctx context.Context, col, id string) error {
	_, err := invokeRouted(r, ctx, "", false, nil, func(c Conn) (bool, error) { return true, c.Delete(ctx, col, id) })
	return err
}
func (r *routedConn) Ping(ctx context.Context) error {
	_, err := invokeRouted(r, ctx, "", false, nil, func(c Conn) (bool, error) { return true, c.Ping(ctx) })
	return err
}
func (r *routedConn) Close() error {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.closed = true
	var errs []error
	for _, client := range r.clients {
		errs = append(errs, client.Close())
	}
	r.clients = map[string]Conn{}
	return errors.Join(errs...)
}

func (r *redwireFacade) topology(ctx context.Context, protocol string) (*contract.Topology, error) {
	data, err := r.conn.Topology(ctx, protocol)
	if err != nil || data == nil {
		return nil, err
	}
	return contract.Decode(data)
}
func (g *grpcFacade) topology(ctx context.Context, protocol string) (*contract.Topology, error) {
	data, err := g.c.Topology(ctx, protocol)
	if err != nil {
		return nil, err
	}
	return contract.Decode(data)
}
func (h *httpFacade) topology(ctx context.Context, protocol string) (*contract.Topology, error) {
	data, err := h.c.Topology(ctx, protocol)
	if err != nil {
		return nil, err
	}
	encoded, err := json.Marshal(data)
	if err != nil {
		return nil, err
	}
	var topology contract.Topology
	err = json.Unmarshal(encoded, &topology)
	return &topology, err
}

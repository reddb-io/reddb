package reddb

import (
	"context"
	"encoding/json"
	"math"
	"net"
	"testing"
	"time"

	pb "github.com/reddb-io/reddb-go/grpcx/proto"
	"github.com/reddb-io/reddb-go/redwire"
	"google.golang.org/grpc"
)

type fakeGrpcServer struct {
	pb.UnimplementedRedDbServer
	t       *testing.T
	lastReq *pb.QueryRequest
}

func (s *fakeGrpcServer) Query(ctx context.Context, req *pb.QueryRequest) (*pb.QueryReply, error) {
	s.lastReq = req
	return &pb.QueryReply{
		Ok:          true,
		Mode:        "query",
		Statement:   req.Query,
		Engine:      "fake",
		Columns:     []string{"ok"},
		RecordCount: 1,
		ResultJson:  `{"records":[{"ok":true}]}`,
	}, nil
}

func TestGrpcQueryWithParamsUsesTypedQueryValues(t *testing.T) {
	srv, addr := startFakeGrpcServer(t)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	c, err := Connect(ctx, "grpc://"+addr)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer c.Close()

	body, err := c.Query(ctx, "SELECT $1, $2, $3, $4, $5",
		int64(42), "alice", nil, []float32{1.5, -2.25}, []byte{0xde, 0xad})
	if err != nil {
		t.Fatalf("query: %v", err)
	}
	if string(body) != `{"records":[{"ok":true}]}` {
		t.Fatalf("body = %q", body)
	}

	req := srv.lastReq
	if req == nil {
		t.Fatal("server did not receive Query")
	}
	if req.Query != "SELECT $1, $2, $3, $4, $5" {
		t.Fatalf("query = %q", req.Query)
	}
	if len(req.Params) != 5 {
		t.Fatalf("params len = %d", len(req.Params))
	}
	if got := req.Params[0].GetIntValue(); got != 42 {
		t.Fatalf("int param = %d", got)
	}
	if got := req.Params[1].GetTextValue(); got != "alice" {
		t.Fatalf("text param = %q", got)
	}
	if req.Params[2].GetNullValue() == nil {
		t.Fatalf("null param kind = %T", req.Params[2].Kind)
	}
	vec := req.Params[3].GetVectorValue().GetValues()
	if len(vec) != 2 || vec[0] != 1.5 || vec[1] != -2.25 {
		t.Fatalf("vector param = %v", vec)
	}
	if got := req.Params[4].GetBytesValue(); string(got) != string([]byte{0xde, 0xad}) {
		t.Fatalf("bytes param = %x", got)
	}
}

func TestGrpcQueryWithParamsCoversNullFloatJsonTimestampAndUUID(t *testing.T) {
	srv, addr := startFakeGrpcServer(t)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	c, err := Connect(ctx, "grpc://"+addr)
	if err != nil {
		t.Fatalf("connect: %v", err)
	}
	defer c.Close()

	u, err := redwire.UUIDFromString("550e8400-e29b-41d4-a716-446655440000")
	if err != nil {
		t.Fatal(err)
	}
	ts := time.Unix(1_700_000_000, 0)
	_, err = c.Query(ctx, "SELECT $1, $2, $3, $4, $5",
		nil, math.Inf(1), map[string]any{"b": true, "a": float64(1)}, ts, u)
	if err != nil {
		t.Fatalf("query: %v", err)
	}

	req := srv.lastReq
	if len(req.Params) != 5 {
		t.Fatalf("params len = %d", len(req.Params))
	}
	if req.Params[0].GetNullValue() == nil {
		t.Fatalf("null param kind = %T", req.Params[0].Kind)
	}
	if got := req.Params[1].GetFloatValue(); !math.IsInf(got, 1) {
		t.Fatalf("float param = %v", got)
	}
	var jsonBody map[string]any
	if err := json.Unmarshal([]byte(req.Params[2].GetJsonValue()), &jsonBody); err != nil {
		t.Fatalf("json param is invalid: %v", err)
	}
	if req.Params[2].GetJsonValue() != `{"a":1,"b":true}` {
		t.Fatalf("json param = %q", req.Params[2].GetJsonValue())
	}
	if got := req.Params[3].GetTimestampValue(); got != ts.Unix() {
		t.Fatalf("timestamp param = %d", got)
	}
	if got := req.Params[4].GetUuidValue(); string(got) != string(u[:]) {
		t.Fatalf("uuid param = %x", got)
	}
}

func startFakeGrpcServer(t *testing.T) (*fakeGrpcServer, string) {
	t.Helper()

	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	server := grpc.NewServer()
	fake := &fakeGrpcServer{t: t}
	pb.RegisterRedDbServer(server, fake)
	go func() {
		if err := server.Serve(lis); err != nil {
			t.Logf("fake grpc server exited: %v", err)
		}
	}()
	t.Cleanup(func() {
		server.Stop()
		_ = lis.Close()
	})
	return fake, lis.Addr().String()
}

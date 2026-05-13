package grpcx

import (
	"context"
	"net"
	"testing"
	"time"

	pb "github.com/reddb-io/reddb-go/grpcx/proto"
	"google.golang.org/grpc"
)

type fakeAskServer struct {
	pb.UnimplementedRedDbServer
	lastReq *pb.AskRequest
}

func (s *fakeAskServer) Ask(ctx context.Context, req *pb.AskRequest) (*pb.AskReply, error) {
	s.lastReq = req
	return &pb.AskReply{
		Answer:          "Lisbon [^1].",
		SourcesFlatJson: `[{"payload":"{\"name\":\"Lisbon\"}","urn":"urn:city:1"}]`,
		Citations: []*pb.Citation{{
			Marker: 1,
			Urn:    "urn:city:1",
		}},
		Validation: &pb.Validation{
			Ok:       true,
			Warnings: []*pb.ValidationItem{},
			Errors:   []*pb.ValidationItem{},
		},
		Provider:         "openai",
		Model:            "gpt-4o-mini",
		PromptTokens:     11,
		CompletionTokens: 3,
		CostUsd:          0.0002,
		CacheHit:         false,
		Mode:             "strict",
		RetryCount:       0,
	}, nil
}

func TestAskRoundTripsTypedGrpcSchema(t *testing.T) {
	srv, addr := startFakeAskServer(t)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	c, err := Dial(ctx, Options{Addr: addr, Plaintext: true})
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer c.Close()

	strict := true
	reply, err := c.Ask(ctx, &pb.AskRequest{
		Question: "What is the capital of Portugal?",
		Strict:   &strict,
	})
	if err != nil {
		t.Fatalf("ask: %v", err)
	}

	if srv.lastReq == nil {
		t.Fatal("server did not receive Ask")
	}
	if got := srv.lastReq.GetQuestion(); got != "What is the capital of Portugal?" {
		t.Fatalf("question = %q", got)
	}
	if srv.lastReq.Strict == nil || !srv.lastReq.GetStrict() {
		t.Fatalf("strict presence/value = %v", srv.lastReq.Strict)
	}
	if got := reply.GetAnswer(); got != "Lisbon [^1]." {
		t.Fatalf("answer = %q", got)
	}
	if got := reply.GetSourcesFlatJson(); got == "" {
		t.Fatal("sources_flat_json is empty")
	}
	if got := reply.GetCitations()[0].GetUrn(); got != "urn:city:1" {
		t.Fatalf("citation urn = %q", got)
	}
	if !reply.GetValidation().GetOk() {
		t.Fatal("validation.ok = false")
	}
	if got := reply.GetMode(); got != "strict" {
		t.Fatalf("mode = %q", got)
	}
}

func startFakeAskServer(t *testing.T) (*fakeAskServer, string) {
	t.Helper()

	lis, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	server := grpc.NewServer()
	fake := &fakeAskServer{}
	pb.RegisterRedDbServer(server, fake)
	go func() {
		if err := server.Serve(lis); err != nil {
			t.Logf("fake grpc ask server exited: %v", err)
		}
	}()
	t.Cleanup(func() {
		server.Stop()
		_ = lis.Close()
	})
	return fake, lis.Addr().String()
}

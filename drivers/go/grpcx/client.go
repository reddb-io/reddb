package grpcx

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"fmt"
	"time"

	pb "github.com/reddb-io/reddb-go/grpcx/proto"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/metadata"
)

type Options struct {
	Addr                  string
	Token                 string
	Timeout               time.Duration
	TLSRootCAs            *x509.CertPool
	TLSCertificates       []tls.Certificate
	TLSInsecureSkipVerify bool
	TLSServerName         string
	Plaintext             bool
}

type Client struct {
	conn   *grpc.ClientConn
	client pb.RedDbClient
	token  string
}

func Dial(ctx context.Context, opts Options) (*Client, error) {
	if opts.Addr == "" {
		return nil, fmt.Errorf("grpc address is required")
	}
	dialCtx := ctx
	cancel := func() {}
	if opts.Timeout > 0 {
		dialCtx, cancel = context.WithTimeout(ctx, opts.Timeout)
	}
	defer cancel()

	dialOpts := []grpc.DialOption{}
	if opts.Plaintext {
		dialOpts = append(dialOpts, grpc.WithTransportCredentials(insecure.NewCredentials()))
	} else {
		cfg := &tls.Config{
			RootCAs:            opts.TLSRootCAs,
			Certificates:       opts.TLSCertificates,
			InsecureSkipVerify: opts.TLSInsecureSkipVerify,
			ServerName:         opts.TLSServerName,
		}
		dialOpts = append(dialOpts, grpc.WithTransportCredentials(credentials.NewTLS(cfg)))
	}

	dialOpts = append(dialOpts, grpc.WithBlock())
	conn, err := grpc.DialContext(dialCtx, opts.Addr, dialOpts...)
	if err != nil {
		return nil, err
	}
	return &Client{
		conn:   conn,
		client: pb.NewRedDbClient(conn),
		token:  opts.Token,
	}, nil
}

func (c *Client) Query(ctx context.Context, sql string, params ...any) (*pb.QueryReply, error) {
	encoded, err := EncodeParams(params)
	if err != nil {
		return nil, err
	}
	ctx = c.authContext(ctx)
	return c.client.Query(ctx, &pb.QueryRequest{
		Query:  sql,
		Params: encoded,
	})
}

func (c *Client) Ping(ctx context.Context) error {
	ctx = c.authContext(ctx)
	_, err := c.client.Health(ctx, &pb.Empty{})
	return err
}

func (c *Client) authContext(ctx context.Context) context.Context {
	if c == nil || c.token == "" {
		return ctx
	}
	return metadata.AppendToOutgoingContext(ctx, "authorization", "Bearer "+c.token)
}

func (c *Client) Close() error {
	if c == nil || c.conn == nil {
		return nil
	}
	return c.conn.Close()
}

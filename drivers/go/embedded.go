package reddb

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"runtime"
	"sync"
	"unsafe"

	"github.com/ebitengine/purego"
)

const (
	embeddedABIVersion = uint32(1)
	maxABIStringBytes  = 256 * 1024 * 1024
)

type nativeABI struct {
	open       func(string, *uintptr, *uintptr) int32
	invoke     func(uintptr, string, string, *uintptr, *uintptr) int32
	close      func(uintptr, *uintptr) int32
	stringFree func(uintptr)
}

var loadNativeABI = sync.OnceValues(func() (_ *nativeABI, err error) {
	defer func() {
		if recovered := recover(); recovered != nil {
			err = fmt.Errorf("bind RedDB embedded ABI: %v", recovered)
		}
	}()

	name := os.Getenv("REDDB_FFI_LIBRARY")
	if name == "" {
		switch runtime.GOOS {
		case "windows":
			name = "reddb_ffi.dll"
		case "darwin":
			name = "libreddb_ffi.dylib"
		default:
			name = "libreddb_ffi.so"
		}
	}
	library, err := openNativeLibrary(name)
	if err != nil {
		return nil, fmt.Errorf("load %s: %w", name, err)
	}

	var version func() uint32
	purego.RegisterLibFunc(&version, library, "reddb_abi_version")
	if got := version(); got != embeddedABIVersion {
		return nil, fmt.Errorf(
			"incompatible RedDB embedded ABI: expected %d, found %d",
			embeddedABIVersion,
			got,
		)
	}

	abi := &nativeABI{}
	purego.RegisterLibFunc(&abi.open, library, "reddb_open")
	purego.RegisterLibFunc(&abi.invoke, library, "reddb_invoke")
	purego.RegisterLibFunc(&abi.close, library, "reddb_close")
	purego.RegisterLibFunc(&abi.stringFree, library, "reddb_string_free")
	return abi, nil
})

func (a *nativeABI) takeString(pointer uintptr) (string, error) {
	if pointer == 0 {
		return "", nil
	}
	defer a.stringFree(pointer)
	start := unsafe.Pointer(pointer)
	for length := 0; length < maxABIStringBytes; length++ {
		if *(*byte)(unsafe.Add(start, length)) == 0 {
			return string(unsafe.Slice((*byte)(start), length)), nil
		}
	}
	return "", NewError(CodeProtocol, "native ABI returned an unterminated string")
}

func (a *nativeABI) openHandle(path string) (uintptr, error) {
	var handle uintptr
	var errorPointer uintptr
	status := a.open(path, &handle, &errorPointer)
	message, takeErr := a.takeString(errorPointer)
	if takeErr != nil {
		return 0, takeErr
	}
	if status != 0 {
		if message == "" {
			message = fmt.Sprintf("reddb_open failed with status %d", status)
		}
		return 0, NewError(CodeEngine, message)
	}
	if handle == 0 {
		return 0, NewError(CodeProtocol, "reddb_open returned a null handle")
	}
	return handle, nil
}

func (a *nativeABI) call(handle uintptr, method string, params []byte) ([]byte, error) {
	var resultPointer uintptr
	var errorPointer uintptr
	status := a.invoke(handle, method, string(params), &resultPointer, &errorPointer)
	message, takeErr := a.takeString(errorPointer)
	if takeErr != nil {
		return nil, takeErr
	}
	if status != 0 {
		if resultPointer != 0 {
			a.stringFree(resultPointer)
		}
		if message == "" {
			message = fmt.Sprintf("reddb_invoke failed with status %d", status)
		}
		return nil, NewError(CodeEngine, message)
	}
	if resultPointer == 0 {
		return nil, NewError(CodeProtocol, "reddb_invoke returned a null result")
	}
	result, err := a.takeString(resultPointer)
	return []byte(result), err
}

type embeddedConn struct {
	mu     sync.Mutex
	abi    *nativeABI
	handle uintptr
}

func connectEmbedded(path string) (Conn, error) {
	abi, err := loadNativeABI()
	if err != nil {
		return nil, WrapError(CodeEmbeddedUnsupported, "embedded native library unavailable", err)
	}
	handle, err := abi.openHandle(path)
	if err != nil {
		return nil, err
	}
	return &embeddedConn{abi: abi, handle: handle}, nil
}

func (c *embeddedConn) invoke(ctx context.Context, method string, params any) ([]byte, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	body, err := json.Marshal(params)
	if err != nil {
		return nil, WrapError(CodeInvalidArgument, "encode embedded parameters", err)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle == 0 {
		return nil, NewError(CodeClosed, "embedded connection is closed")
	}
	return c.abi.call(c.handle, method, body)
}

func (c *embeddedConn) Query(ctx context.Context, sql string, params ...any) ([]byte, error) {
	body := map[string]any{"sql": sql}
	if len(params) > 0 {
		body["params"] = params
	}
	return c.invoke(ctx, "query", body)
}

func (c *embeddedConn) Exec(ctx context.Context, sql string, params ...any) (Result, error) {
	body, err := c.Query(ctx, sql, params...)
	if err != nil {
		return Result{}, err
	}
	return execResultFromJSON(body)
}

func (c *embeddedConn) Insert(ctx context.Context, collection string, payload any) error {
	_, err := c.invoke(ctx, "insert", map[string]any{
		"collection": collection,
		"payload":    payload,
	})
	return err
}

func (c *embeddedConn) BulkInsert(
	ctx context.Context,
	collection string,
	rows []any,
) (*BulkInsertResult, error) {
	body, err := c.invoke(ctx, "bulk_insert", map[string]any{
		"collection": collection,
		"payloads":   rows,
	})
	if err != nil {
		return nil, err
	}
	var result BulkInsertResult
	if err := json.Unmarshal(body, &result); err != nil {
		return nil, WrapError(CodeInvalidResponse, "decode embedded bulk result", err)
	}
	return &result, nil
}

func (c *embeddedConn) Get(ctx context.Context, collection, id string) ([]byte, error) {
	return c.invoke(ctx, "get", map[string]any{"collection": collection, "id": id})
}

func (c *embeddedConn) Delete(ctx context.Context, collection, id string) error {
	_, err := c.invoke(ctx, "delete", map[string]any{"collection": collection, "id": id})
	return err
}

func (c *embeddedConn) Ping(ctx context.Context) error {
	_, err := c.invoke(ctx, "health", map[string]any{})
	return err
}

func (c *embeddedConn) Close() error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.handle == 0 {
		return nil
	}
	handle := c.handle
	c.handle = 0
	var errorPointer uintptr
	status := c.abi.close(handle, &errorPointer)
	message, err := c.abi.takeString(errorPointer)
	if err != nil {
		return err
	}
	if status != 0 {
		if message == "" {
			message = fmt.Sprintf("reddb_close failed with status %d", status)
		}
		return NewError(CodeEngine, message)
	}
	return nil
}

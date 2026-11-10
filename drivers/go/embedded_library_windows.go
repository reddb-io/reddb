//go:build windows

package reddb

import "syscall"

func openNativeLibrary(name string) (uintptr, error) {
	handle, err := syscall.LoadLibrary(name)
	return uintptr(handle), err
}

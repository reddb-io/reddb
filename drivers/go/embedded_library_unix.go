//go:build !windows

package reddb

import "github.com/ebitengine/purego"

func openNativeLibrary(name string) (uintptr, error) {
	return purego.Dlopen(name, purego.RTLD_NOW|purego.RTLD_LOCAL)
}

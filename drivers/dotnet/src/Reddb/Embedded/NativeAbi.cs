using System;
using System.Reflection;
using System.Runtime.InteropServices;

namespace Reddb.Embedded;

internal sealed class NativeAbi
{
    private const uint ExpectedVersion = 1;

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate uint AbiVersionFn();

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int OpenFn(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? path,
        out IntPtr handle,
        out IntPtr error);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int InvokeFn(
        IntPtr handle,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string method,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string parameters,
        out IntPtr result,
        out IntPtr error);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate int CloseFn(IntPtr handle, out IntPtr error);

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    private delegate void StringFreeFn(IntPtr value);

    private readonly OpenFn _open;
    private readonly InvokeFn _invoke;
    private readonly CloseFn _close;
    private readonly StringFreeFn _stringFree;

    private NativeAbi(IntPtr library)
    {
        var version = Bind<AbiVersionFn>(library, "reddb_abi_version")();
        if (version != ExpectedVersion)
        {
            throw new NotSupportedException(
                $"incompatible RedDB embedded ABI: expected {ExpectedVersion}, found {version}");
        }
        _open = Bind<OpenFn>(library, "reddb_open");
        _invoke = Bind<InvokeFn>(library, "reddb_invoke");
        _close = Bind<CloseFn>(library, "reddb_close");
        _stringFree = Bind<StringFreeFn>(library, "reddb_string_free");
    }

    internal static NativeAbi Load()
    {
        string? configured = Environment.GetEnvironmentVariable("REDDB_FFI_LIBRARY");
        IntPtr library;
        if (!string.IsNullOrWhiteSpace(configured))
        {
            library = NativeLibrary.Load(configured);
        }
        else if (!NativeLibrary.TryLoad(
            "reddb_ffi",
            Assembly.GetExecutingAssembly(),
            searchPath: null,
            out library))
        {
            throw new DllNotFoundException(
                "RedDB embedded library was not found. Install the native package or set " +
                "REDDB_FFI_LIBRARY to reddb_ffi.dll, libreddb_ffi.so, or libreddb_ffi.dylib.");
        }
        return new NativeAbi(library);
    }

    internal IntPtr Open(string? path)
    {
        int status = _open(path, out IntPtr handle, out IntPtr error);
        string message = TakeString(error);
        if (status != 0)
        {
            throw new RedDBException.EngineError(
                message.Length == 0 ? $"reddb_open failed with status {status}" : message);
        }
        if (handle == IntPtr.Zero)
        {
            throw new RedDBException.ProtocolError("reddb_open returned a null handle");
        }
        return handle;
    }

    internal byte[] Invoke(IntPtr handle, string method, string parameters)
    {
        int status = _invoke(handle, method, parameters, out IntPtr result, out IntPtr error);
        string message = TakeString(error);
        if (status != 0)
        {
            if (result != IntPtr.Zero) _stringFree(result);
            throw new RedDBException.EngineError(
                message.Length == 0 ? $"reddb_invoke failed with status {status}" : message);
        }
        if (result == IntPtr.Zero)
        {
            throw new RedDBException.ProtocolError("reddb_invoke returned a null result");
        }
        string json = TakeString(result);
        return System.Text.Encoding.UTF8.GetBytes(json);
    }

    internal void Close(IntPtr handle)
    {
        int status = _close(handle, out IntPtr error);
        string message = TakeString(error);
        if (status != 0)
        {
            throw new RedDBException.EngineError(
                message.Length == 0 ? $"reddb_close failed with status {status}" : message);
        }
    }

    private string TakeString(IntPtr value)
    {
        if (value == IntPtr.Zero) return string.Empty;
        try
        {
            return Marshal.PtrToStringUTF8(value) ?? string.Empty;
        }
        finally
        {
            _stringFree(value);
        }
    }

    private static T Bind<T>(IntPtr library, string symbol) where T : Delegate
    {
        IntPtr address = NativeLibrary.GetExport(library, symbol);
        return Marshal.GetDelegateForFunctionPointer<T>(address);
    }
}

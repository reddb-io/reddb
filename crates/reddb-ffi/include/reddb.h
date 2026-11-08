#ifndef REDDB_H
#define REDDB_H

#include <stdint.h>

#if defined(_WIN32) && !defined(REDDB_STATIC)
#if defined(REDDB_BUILD)
#define REDDB_API __declspec(dllexport)
#else
#define REDDB_API __declspec(dllimport)
#endif
#else
#define REDDB_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef struct reddb_handle reddb_handle;

typedef enum reddb_status {
    REDDB_OK = 0,
    REDDB_INVALID_ARGUMENT = 1,
    REDDB_OPEN_ERROR = 2,
    REDDB_CALL_ERROR = 3,
    REDDB_INTERNAL_ERROR = 4
} reddb_status;

/* ABI version of this header/library pair. */
REDDB_API uint32_t reddb_abi_version(void);

/*
 * Open an in-process engine. A NULL or empty UTF-8 path creates an in-memory
 * database; every other path names the one durable .rdb file.
 *
 * On success, *out_handle is owned by the caller and must be passed exactly
 * once to reddb_close. On failure, *out_error is either NULL or an allocated
 * UTF-8 string that must be released with reddb_string_free.
 */
REDDB_API reddb_status reddb_open(
    const char *path,
    reddb_handle **out_handle,
    char **out_error
);

/*
 * Execute one JSON-RPC 2.0 request against the embedded session. Calls on one
 * handle are serialized. The response and error outputs use
 * reddb_string_free. A successful "close" RPC checkpoints and closes the
 * engine, but the handle itself must still be released with reddb_close.
 */
REDDB_API reddb_status reddb_call(
    reddb_handle *handle,
    const char *request,
    char **out_response,
    char **out_error
);

/*
 * Checkpoint, close, and release a handle. The handle is invalid after this
 * call even when checkpointing reports an error. Callers must ensure no other
 * thread is using the handle concurrently.
 */
REDDB_API reddb_status reddb_close(reddb_handle *handle, char **out_error);

/* Release a string returned through out_response or out_error. NULL is safe. */
REDDB_API void reddb_string_free(char *value);

#ifdef __cplusplus
}
#endif

#endif

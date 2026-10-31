#import "GeneratedPluginRegistrant.h"

#include <stddef.h>
#include <stdint.h>
// 0 completed, 1 pending, 2 failed. Pending/failure remain gated.
int32_t cp_account_suspend(void);
int32_t cp_account_resume(void);

// Hands Runner's Keychain functions to Rust, which cannot link against Runner.
// 0 registered, 1 already registered, 3 a null pointer.
typedef int32_t (*cp_key_transform)(const uint8_t *id, const uint8_t *input, size_t len,
                                    uint8_t *out, size_t cap, size_t *out_len);
typedef int32_t (*cp_key_remove)(const uint8_t *id);
int32_t cp_key_register(cp_key_transform protect, cp_key_transform unprotect, cp_key_remove remove);

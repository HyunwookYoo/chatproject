use super::{error, status, validate_blob, MAX_BLOB};
use chat_core::{
    account::{storage::KeyProtector, Secret32, StoreId},
    CoreError, ErrorCode,
};
use jni::objects::JValue;
use std::sync::Arc;
use zeroize::Zeroizing;

struct AndroidProtector;

// Instrumentation-only: absent from every production build and release profile.
#[cfg(all(debug_assertions, feature = "test-hooks"))]
#[no_mangle]
pub extern "system" fn Java_dev_chatproject_chat_1app_AccountKeyStoreTest_nativeWorkerRoundTrip(
    env: jni::JNIEnv,
    _this: jni::objects::JObject,
    id: jni::objects::JByteArray,
) -> jni::sys::jboolean {
    let Ok(bytes) = env.convert_byte_array(id) else {
        return jni::sys::JNI_FALSE;
    };
    let Ok(scope) = <[u8; 16]>::try_from(bytes.as_slice()) else {
        return jni::sys::JNI_FALSE;
    };
    let result = std::thread::spawn(move || -> Result<bool, CoreError> {
        let provider = provider()?;
        let id = StoreId(scope);
        // Synthetic fixture only; neither production secrets nor bytes cross this test boundary.
        let secret = Secret32::from([0x62; 32]);
        let check = (|| {
            let blob = provider.protect(id, &secret)?;
            let opened = provider.unprotect(id, &blob)?;
            if opened.as_bytes() != secret.as_bytes() {
                return Ok(false);
            }
            let mut changed = blob.clone();
            let last = changed.len() - 1;
            changed[last] ^= 1;
            if provider.unprotect(id, &changed).err().map(|e| e.code)
                != Some(ErrorCode::StorageCorrupt)
            {
                return Ok(false);
            }
            provider.delete(id)?;
            Ok(provider.unprotect(id, &blob).err().map(|e| e.code)
                == Some(ErrorCode::StorageMissing))
        })();
        let deleted = provider.delete(id);
        deleted?;
        check
    })
    .join();
    if matches!(result, Ok(Ok(true))) {
        jni::sys::JNI_TRUE
    } else {
        jni::sys::JNI_FALSE
    }
}

pub(super) fn provider() -> Result<Arc<dyn KeyProtector>, CoreError> {
    crate::android_jni::account_context().ok_or_else(|| error(ErrorCode::NotAvailable))?;
    Ok(Arc::new(AndroidProtector))
}

fn call(method: &str, id: StoreId, input: &[u8]) -> Result<Zeroizing<Vec<u8>>, CoreError> {
    let (vm, helper) =
        crate::android_jni::account_context().ok_or_else(|| error(ErrorCode::NotAvailable))?;
    // Every caller gets its own thread-local JNIEnv. The cached object owns its class loader.
    let mut env = vm
        .attach_current_thread()
        .map_err(|_| error(ErrorCode::NotAvailable))?;
    let result = env.with_local_frame(16, |env| -> jni::errors::Result<_> {
        let scope = env.byte_array_from_slice(&id.0)?;
        let out = env.new_byte_array(if method == "protect" {
            MAX_BLOB as i32
        } else {
            32
        })?;
        let length = env.new_int_array(1)?;
        let source = env.byte_array_from_slice(input)?;
        let result = (|| -> jni::errors::Result<_> {
            let code = env
                .call_method(
                    helper.as_obj(),
                    method,
                    "([B[B[B[I)I",
                    &[
                        JValue::Object(&scope),
                        JValue::Object(&source),
                        JValue::Object(&out),
                        JValue::Object(&length),
                    ],
                )?
                .i()?;
            let mut count = [0];
            env.get_int_array_region(&length, 0, &mut count)?;
            let bytes = Zeroizing::new(env.convert_byte_array(&out)?);
            Ok((code, count[0], bytes))
        })();
        // Clear native exceptions before erasing temporary JVM plaintext arrays. Never log details.
        if env.exception_check()? {
            env.exception_clear()?;
        }
        if method == "protect" {
            env.set_byte_array_region(&source, 0, &[0; 32])?;
        } else {
            env.set_byte_array_region(&out, 0, &[0; 32])?;
        }
        result
    });
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
    let (code, length, mut bytes) = result.map_err(|_| error(ErrorCode::NotAvailable))?;
    status(code)?;
    if length <= 0 || length as usize > bytes.len() {
        return Err(error(ErrorCode::StorageCorrupt));
    }
    bytes.truncate(length as usize);
    Ok(bytes)
}

impl KeyProtector for AndroidProtector {
    fn protect(&self, id: StoreId, secret: &Secret32) -> Result<Vec<u8>, CoreError> {
        Ok(call("protect", id, secret.as_bytes())?.to_vec())
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        validate_blob(blob)?;
        let bytes = call("unprotect", id, blob)?;
        let key: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| error(ErrorCode::StorageCorrupt))?;
        Ok(Secret32::from(key))
    }
    fn delete(&self, id: StoreId) -> Result<(), CoreError> {
        let (vm, helper) =
            crate::android_jni::account_context().ok_or_else(|| error(ErrorCode::NotAvailable))?;
        let mut env = vm
            .attach_current_thread()
            .map_err(|_| error(ErrorCode::NotAvailable))?;
        let result = env.with_local_frame(4, |env| -> jni::errors::Result<_> {
            let scope = env.byte_array_from_slice(&id.0)?;
            env.call_method(
                helper.as_obj(),
                "delete",
                "([B)I",
                &[JValue::Object(&scope)],
            )?
            .i()
        });
        if env.exception_check().unwrap_or(false) {
            let _ = env.exception_clear();
        }
        status(result.map_err(|_| error(ErrorCode::NotAvailable))?)
    }
}

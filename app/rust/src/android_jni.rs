use jni::{
    objects::{GlobalRef, JObject},
    sys::{jboolean, jint, JNI_ERR, JNI_FALSE, JNI_TRUE, JNI_VERSION_1_6},
    JNIEnv, JavaVM,
};
use std::{
    ffi::c_void,
    io::Write,
    sync::{Mutex, OnceLock},
};

// These handles must stay alive until process exit, including Activity recreation.
#[allow(dead_code)]
struct ContextState {
    vm: JavaVM,
    context: GlobalRef,
}

static JVM_PTR: OnceLock<usize> = OnceLock::new();
static CONTEXT: OnceLock<ContextState> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());
static ACCOUNT_KEYS: OnceLock<GlobalRef> = OnceLock::new();

pub(crate) fn account_context() -> Option<(&'static JavaVM, &'static GlobalRef)> {
    Some((&CONTEXT.get()?.vm, ACCOUNT_KEYS.get()?))
}

// Called from managed initialization after the existing network VM/context setup.
// Cache the application object here: native worker threads cannot FindClass it reliably.
#[no_mangle]
pub extern "system" fn Java_dev_chatproject_chat_1app_MainActivity_initializeAccountKeyStore(
    env: JNIEnv,
    _this: JObject,
    store: JObject,
) -> jboolean {
    if CONTEXT.get().is_none() || store.is_null() {
        return JNI_FALSE;
    }
    if ACCOUNT_KEYS.get().is_some() {
        return JNI_TRUE;
    }
    match env.new_global_ref(store) {
        Ok(reference) => {
            let _ = ACCOUNT_KEYS.set(reference);
            JNI_TRUE
        }
        Err(_) => JNI_FALSE,
    }
}

#[no_mangle]
pub extern "system" fn JNI_OnLoad(vm: *mut jni::sys::JavaVM, _reserved: *mut c_void) -> jint {
    if vm.is_null() {
        return JNI_ERR;
    }
    match JVM_PTR.get() {
        Some(saved) if *saved != vm as usize => JNI_ERR,
        Some(_) => JNI_VERSION_1_6,
        None => {
            if JVM_PTR.set(vm as usize).is_err() {
                return JNI_ERR;
            }
            JNI_VERSION_1_6
        }
    }
}

fn initialization_failed() -> jboolean {
    let _ = writeln!(std::io::stderr(), "Network context initialization failed");
    JNI_FALSE
}

#[no_mangle]
pub extern "system" fn Java_dev_chatproject_chat_1app_MainActivity_initializeNetworkContext(
    env: JNIEnv,
    _this: JObject,
    context: JObject,
) -> jboolean {
    let _init = match INIT.lock() {
        Ok(guard) => guard,
        Err(_) => return initialization_failed(),
    };
    if CONTEXT.get().is_some() {
        return JNI_TRUE;
    }
    if context.is_null() {
        return initialization_failed();
    }
    let Some(ptr) = JVM_PTR.get() else {
        return initialization_failed();
    };
    let vm = match unsafe { JavaVM::from_raw(*ptr as *mut jni::sys::JavaVM) } {
        Ok(vm) => vm,
        Err(_) => return initialization_failed(),
    };
    let global = match env.new_global_ref(context) {
        Ok(global) => global,
        Err(_) => return initialization_failed(),
    };
    // The initialization lock makes this the sole writer. iroh initializes ndk-context.
    unsafe {
        iroh::dns::install_android_jni_context(
            vm.get_java_vm_pointer().cast(),
            global.as_obj().as_raw().cast(),
        );
    }
    let _ = CONTEXT.set(ContextState {
        vm,
        context: global,
    });
    JNI_TRUE
}

//! The VM that loaded us and the bridge class in it.
//!
//! NSH's host keeps its own `JavaVM` to itself, so the VM is found with
//! `JNI_GetCreatedJavaVMs` (libnativehelper, public since Android 12; the
//! process's own symbols before that). Threads Rust made see the system
//! class loader only: the bridge's dex is loaded into an
//! `InMemoryDexClassLoader` of its own and its class kept.

use std::ffi::c_void;
use std::sync::OnceLock;

use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JValue};
use jni::sys::{JNI_OK, jint, jsize};
use jni::{JNIEnv, JavaVM};

/// The bridge, compiled by `build.rs`.
static DEX: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/classes.dex"));
const BRIDGE_CLASS: &str = "com.clipboardx.Bridge";
/// Local references a call makes, at most.
const LOCAL_FRAME: i32 = 16;

struct Bridge {
    vm: JavaVM,
    class: GlobalRef,
}

static BRIDGE: OnceLock<Result<Bridge, String>> = OnceLock::new();

type GetCreatedVms =
    unsafe extern "system" fn(*mut *mut jni::sys::JavaVM, jsize, *mut jsize) -> jint;

fn find_symbol() -> Option<GetCreatedVms> {
    let name = c"JNI_GetCreatedJavaVMs";
    // SAFETY: dlopen/dlsym with NUL-terminated names; the library stays
    // loaded (never closed), so the symbol stays valid.
    let symbol = unsafe {
        let helper = libc::dlopen(c"libnativehelper.so".as_ptr(), libc::RTLD_NOW);
        let symbol = if helper.is_null() {
            std::ptr::null_mut()
        } else {
            libc::dlsym(helper, name.as_ptr())
        };
        if symbol.is_null() {
            libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr())
        } else {
            symbol
        }
    };
    // SAFETY: the symbol is JNI's `JNI_GetCreatedJavaVMs`, of this type.
    (!symbol.is_null())
        .then(|| unsafe { std::mem::transmute::<*mut c_void, GetCreatedVms>(symbol) })
}

fn find_vm() -> Result<JavaVM, String> {
    let get = find_symbol().ok_or("JNI_GetCreatedJavaVMs not found")?;
    let mut vm = std::ptr::null_mut();
    let mut count: jsize = 0;
    // SAFETY: room for one VM pointer and the count, as JNI asks.
    let status = unsafe { get(&mut vm, 1, &mut count) };
    if status != JNI_OK || count < 1 || vm.is_null() {
        return Err(format!("no Java VM (status {status}, {count} VMs)"));
    }
    // SAFETY: a live VM pointer from JNI; Android has one VM per process.
    unsafe { JavaVM::from_raw(vm) }.map_err(|e| e.to_string())
}

fn load_class(env: &mut JNIEnv<'_>) -> jni::errors::Result<GlobalRef> {
    let dex = env.byte_array_from_slice(DEX)?;
    let buffer = env
        .call_static_method(
            "java/nio/ByteBuffer",
            "wrap",
            "([B)Ljava/nio/ByteBuffer;",
            &[JValue::Object(&dex)],
        )?
        .l()?;
    let parent = env
        .call_static_method(
            "java/lang/ClassLoader",
            "getSystemClassLoader",
            "()Ljava/lang/ClassLoader;",
            &[],
        )?
        .l()?;
    let loader = env.new_object(
        "dalvik/system/InMemoryDexClassLoader",
        "(Ljava/nio/ByteBuffer;Ljava/lang/ClassLoader;)V",
        &[JValue::Object(&buffer), JValue::Object(&parent)],
    )?;
    let name = env.new_string(BRIDGE_CLASS)?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )?
        .l()?;
    env.new_global_ref(class)
}

fn bridge() -> Result<&'static Bridge, String> {
    BRIDGE
        .get_or_init(|| {
            let vm = find_vm()?;
            let class = {
                let mut env = vm.attach_current_thread().map_err(|e| e.to_string())?;
                in_frame(&mut env, load_class)?
            };
            log::info!("Android bridge loaded");
            Ok(Bridge { vm, class })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Runs `f` in a local frame of its own; a Java exception becomes its
/// message.
fn in_frame<T>(
    env: &mut JNIEnv<'_>,
    f: impl FnOnce(&mut JNIEnv<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    match env.with_local_frame(LOCAL_FRAME, f) {
        Ok(value) => Ok(value),
        Err(jni::errors::Error::JavaException) => Err(take_exception(env)),
        Err(e) => Err(e.to_string()),
    }
}

/// The pending exception's message (its class when it has none), cleared.
fn take_exception(env: &mut JNIEnv<'_>) -> String {
    let Ok(throwable) = env.exception_occurred() else {
        return "Java exception".into();
    };
    let _ = env.exception_clear();
    if throwable.is_null() {
        return "Java exception".into();
    }
    let text = |env: &mut JNIEnv<'_>, method: &str| -> Option<String> {
        let value = env
            .call_method(&throwable, method, "()Ljava/lang/String;", &[])
            .ok()?
            .l()
            .ok()?;
        if value.is_null() {
            return None;
        }
        let text = env.get_string(&value.into()).ok()?.into();
        Some(text)
    };
    let message = text(env, "getMessage")
        .or_else(|| text(env, "toString"))
        .unwrap_or_else(|| "Java exception".into());
    let _ = env.exception_clear();
    let _ = env.delete_local_ref(JObject::from(throwable));
    message
}

/// Calls the bridge on this thread (attached for the call when Rust made
/// it): `f` gets the environment and the bridge class.
pub fn call<T>(
    f: impl FnOnce(&mut JNIEnv<'_>, &JClass<'_>) -> jni::errors::Result<T>,
) -> Result<T, String> {
    let bridge = bridge()?;
    let mut env = bridge
        .vm
        .attach_current_thread()
        .map_err(|e| e.to_string())?;
    let class: &JClass<'_> = bridge.class.as_obj().into();
    in_frame(&mut env, |env| f(env, class))
}

/// Overwrites the first `len` bytes of `array` with zeros; an exception
/// pending before stays pending (JNI allows no other call with one).
pub fn zero(env: &mut JNIEnv<'_>, array: &JByteArray<'_>, len: usize) {
    let pending = env.exception_occurred().ok().filter(|t| !t.is_null());
    if pending.is_some() {
        let _ = env.exception_clear();
    }
    let zeros = vec![0i8; len];
    if env.set_byte_array_region(array, 0, &zeros).is_err() {
        let _ = env.exception_clear();
    }
    if let Some(thrown) = pending {
        let _ = env.throw(thrown);
    }
}

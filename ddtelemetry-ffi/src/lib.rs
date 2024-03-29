// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.

use ddcommon_ffi as ffi;
use ddtelemetry::worker::{TelemetryWorkerBuilder, TelemetryWorkerHandle};
use ffi::slice::AsBytes;

#[cfg(unix)]
pub mod unix;

macro_rules! c_setters {
    ($object_name:ident, $object_ty:ty, $input_type:ty, $convert_fn:expr, SETTERS { $($path:ident $(. $path_rest:ident)*),+ $(,)? }) => {
        paste::paste! {
            $(
                #[no_mangle]
                #[allow(clippy::redundant_closure_call)]
                #[allow(clippy::missing_safety_doc)]
                pub unsafe extern "C" fn [<ddog_ $object_name _with_ $path $(_ $path_rest)* >](
                    $object_name: &mut $object_ty,
                    param: $input_type,
                ) -> MaybeError {
                    $object_name . $path $(.  $path_rest)* = Some(try_c!($convert_fn (param)));
                    MaybeError::None
                }
            )+

            #[repr(C)]
            #[allow(dead_code)]
            pub enum [<$object_ty Property >] {
                $([< $path:camel $($path_rest:camel)* >],)+
            }

            #[no_mangle]
            #[allow(clippy::redundant_closure_call)]
            #[allow(clippy::missing_safety_doc)]
            #[doc=concat!(
                " Sets a property from it's string value.\n\n",
                " # Available properties:\n\n",
                $(" * ", stringify!($path $(. $path_rest)*) , "\n\n",)+
            )]
            pub unsafe extern "C" fn [<ddog_ $object_name _with_property >](
                $object_name: &mut $object_ty,
                property: [<$object_ty Property >],
                param: $input_type,
            ) -> MaybeError {
                use [<$object_ty Property >] ::*;
                match property {
                    $(
                        [< $path:camel $($path_rest:camel)* >] => {
                            $object_name . $path $(.  $path_rest)* = Some(try_c!($convert_fn (param)));
                        }
                    )+
                }
                MaybeError::None
            }

            #[no_mangle]
            #[allow(clippy::redundant_closure_call)]
            #[allow(clippy::missing_safety_doc)]
            #[doc=concat!(
                " Sets a property from it's string value.\n\n",
                " # Available properties:\n\n",
                $(
                    " * ", stringify!($path $(. $path_rest)*) , "\n\n",
                )+
            )]
            pub unsafe extern "C" fn [<ddog_ $object_name _with_str_property >](
                $object_name: &mut $object_ty,
                property: ffi::CharSlice,
                param: $input_type,
            ) -> MaybeError {
                let property = try_c!(property.try_to_utf8());
                match property {
                    $(
                        stringify!($path $(. $path_rest)*) => {
                            $object_name . $path $(.  $path_rest)* = Some(try_c!($convert_fn (param)));
                        }
                    )+
                    // TODO this is an error
                    _ => return MaybeError::None,
                }
                MaybeError::None
            }
        }

    };
}

type MaybeError = ffi::Option<ffi::Vec<u8>>;

#[no_mangle]
pub extern "C" fn ddog_MaybeError_drop(_: MaybeError) {}

#[macro_export]
macro_rules! try_c {
    ($failable:expr) => {
        match $failable {
            Ok(o) => o,
            Err(e) => return MaybeError::Some(ddcommon_ffi::Vec::from(e.to_string().into_bytes())),
        }
    };
}

/// # Safety
/// * builder should be a non null pointer to a null pointer to a builder
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_instantiate(
    builder: &mut *mut TelemetryWorkerBuilder,
    service_name: ffi::CharSlice,
    language_name: ffi::CharSlice,
    language_version: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
) -> MaybeError {
    let new = Box::new(TelemetryWorkerBuilder::new_fetch_host(
        service_name.to_utf8_lossy().into_owned(),
        language_name.to_utf8_lossy().into_owned(),
        language_version.to_utf8_lossy().into_owned(),
        tracer_version.to_utf8_lossy().into_owned(),
    ));
    // Leaking is the last thing we do before returning
    // Otherwise we would need to manually drop it in case of error
    *builder = Box::into_raw(new);
    MaybeError::None
}

#[no_mangle]
/// # Safety
/// * builder should be a non null pointer to a null pointer to a builder
pub unsafe extern "C" fn ddog_builder_instantiate_with_hostname(
    builder: &mut *mut TelemetryWorkerBuilder,
    hostname: ffi::CharSlice,
    service_name: ffi::CharSlice,
    language_name: ffi::CharSlice,
    language_version: ffi::CharSlice,
    tracer_version: ffi::CharSlice,
) -> MaybeError {
    let new = Box::new(TelemetryWorkerBuilder::new(
        hostname.to_utf8_lossy().into_owned(),
        service_name.to_utf8_lossy().into_owned(),
        language_name.to_utf8_lossy().into_owned(),
        language_version.to_utf8_lossy().into_owned(),
        tracer_version.to_utf8_lossy().into_owned(),
    ));

    // Leaking is the last thing we do before returning
    // Otherwise we would need to manually drop it in case of error
    *builder = Box::into_raw(new);
    MaybeError::None
}

c_setters!(
    builder,
    TelemetryWorkerBuilder,
    ffi::CharSlice, (|s: ffi::CharSlice| -> Result<_, String> { Ok(s.to_utf8_lossy().into_owned()) }),
    SETTERS {
        application.service_version,
        application.env,
        application.runtime_name,
        application.runtime_version,
        application.runtime_patches,

        host.container_id,
        host.os,
        host.kernel_name,
        host.kernel_release,
        host.kernel_version,

        runtime_id
    }
);

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_with_native_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_native_deps: bool,
) -> MaybeError {
    builder.native_deps = include_native_deps;
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_with_rust_shared_lib_deps(
    builder: &mut TelemetryWorkerBuilder,
    include_rust_shared_lib_deps: bool,
) -> MaybeError {
    builder.rust_shared_lib_deps = include_rust_shared_lib_deps;
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_builder_with_config(
    builder: &mut TelemetryWorkerBuilder,
    name: ffi::CharSlice,
    value: ffi::CharSlice,
) -> MaybeError {
    let name = name.to_utf8_lossy().into_owned();
    let value = value.to_utf8_lossy().into_owned();
    builder.library_config.push((name, value));
    MaybeError::None
}

#[no_mangle]
/// # Safety
/// * handle should be a non null pointer to a null pointer
pub unsafe extern "C" fn ddog_builder_run(
    builder: Box<TelemetryWorkerBuilder>,
    handle: *mut Box<TelemetryWorkerHandle>,
) -> MaybeError {
    handle.write(Box::new(try_c!(builder.run())));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_handle_add_dependency(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
) -> MaybeError {
    let name = dependency_name.to_utf8_lossy().into_owned();
    let version = dependency_version
        .is_empty()
        .then(|| dependency_version.to_utf8_lossy().into_owned());
    try_c!(handle.add_dependency(name, version));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_handle_add_integration(
    handle: &TelemetryWorkerHandle,
    dependency_name: ffi::CharSlice,
    dependency_version: ffi::CharSlice,
    compatible: ffi::Option<bool>,
    enabled: ffi::Option<bool>,
    auto_enabled: ffi::Option<bool>,
) -> MaybeError {
    let name = dependency_name.to_utf8_lossy().into_owned();
    let version = dependency_version
        .is_empty()
        .then(|| dependency_version.to_utf8_lossy().into_owned());
    try_c!(handle.add_integration(
        name,
        version,
        compatible.into(),
        enabled.into(),
        auto_enabled.into(),
    ));
    MaybeError::None
}

#[allow(clippy::missing_safety_doc)]
#[no_mangle]
pub unsafe extern "C" fn ddog_handle_add_log(
    handle: &TelemetryWorkerHandle,
    indentifier: ffi::CharSlice,
    message: ffi::CharSlice,
    level: ddtelemetry::data::LogLevel,
    stack_trace: ffi::CharSlice,
) -> MaybeError {
    try_c!(handle.add_log(
        indentifier.as_bytes(),
        message.to_utf8_lossy().into_owned(),
        level,
        stack_trace
            .is_empty()
            .then(|| stack_trace.to_utf8_lossy().into_owned()),
    ));
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_handle_start(handle: &TelemetryWorkerHandle) -> MaybeError {
    try_c!(handle.send_start());
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_handle_clone(handle: &TelemetryWorkerHandle) -> Box<TelemetryWorkerHandle> {
    Box::new(handle.clone())
}

#[no_mangle]
pub extern "C" fn ddog_handle_stop(handle: &TelemetryWorkerHandle) -> MaybeError {
    try_c!(handle.send_stop());
    MaybeError::None
}

#[no_mangle]
pub extern "C" fn ddog_handle_wait_for_shutdown(handle: Box<TelemetryWorkerHandle>) {
    handle.wait_for_shutdown()
}

#[no_mangle]
pub extern "C" fn ddog_handle_drop(handle: Box<TelemetryWorkerHandle>) {
    drop(handle);
}

#[cfg(test)]
mod test_c_setters {
    use super::*;

    #[test]
    fn test_set_builder_str_param() {
        let mut builder = std::ptr::null_mut();

        unsafe {
            assert_eq!(
                ddog_builder_instantiate(
                    &mut builder,
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None
            );
            assert!(!builder.is_null());
            let mut builder = Box::from_raw(builder);

            assert_eq!(
                ddog_builder_with_str_property(
                    &mut builder,
                    ffi::CharSlice::from("runtime_id"),
                    ffi::CharSlice::from("abcd")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.runtime_id.as_deref(), Some("abcd"));

            assert_eq!(
                ddog_builder_with_str_property(
                    &mut builder,
                    ffi::CharSlice::from("application.runtime_name"),
                    ffi::CharSlice::from("rust")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.application.runtime_name.as_deref(), Some("rust"));

            assert_eq!(
                ddog_builder_with_str_property(
                    &mut builder,
                    ffi::CharSlice::from("host.kernel_version"),
                    ffi::CharSlice::from("ダタドグ")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.host.kernel_version.as_deref(), Some("ダタドグ"));

            assert!(ddog_builder_with_str_property(
                &mut builder,
                ffi::CharSlice::from("doesnt exist"),
                ffi::CharSlice::from("abc")
            )
            .to_std()
            .is_none(),);
        }
    }

    #[test]
    fn test_set_builder_enum_param() {
        let mut builder = std::ptr::null_mut();

        unsafe {
            assert_eq!(
                ddog_builder_instantiate(
                    &mut builder,
                    ffi::CharSlice::from("service_name"),
                    ffi::CharSlice::from("language_name"),
                    ffi::CharSlice::from("language_version"),
                    ffi::CharSlice::from("tracer_version"),
                ),
                MaybeError::None,
            );
            assert!(!builder.is_null());
            let mut builder = Box::from_raw(builder);

            assert_eq!(
                ddog_builder_with_property(
                    &mut builder,
                    TelemetryWorkerBuilderProperty::RuntimeId,
                    ffi::CharSlice::from("abcd")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.runtime_id.as_deref(), Some("abcd"));

            assert_eq!(
                ddog_builder_with_property(
                    &mut builder,
                    TelemetryWorkerBuilderProperty::ApplicationRuntimeName,
                    ffi::CharSlice::from("rust")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.application.runtime_name.as_deref(), Some("rust"));

            assert_eq!(
                ddog_builder_with_property(
                    &mut builder,
                    TelemetryWorkerBuilderProperty::HostKernelVersion,
                    ffi::CharSlice::from("ダタドグ")
                ),
                MaybeError::None,
            );
            assert_eq!(builder.host.kernel_version.as_deref(), Some("ダタドグ"));
        }
    }
}

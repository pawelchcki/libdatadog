// Unless explicitly stated otherwise all files in this repository are licensed under the Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2021-Present Datadog, Inc.
use std::{
    ffi::{CStr, CString},
    ptr,
};

use nix::libc;
use smallvec::SmallVec;

use self::ext::CStrExt;

pub mod raw_env {
    use nix::libc;

    use super::CListMutPtr;
    /// # Safety
    ///
    /// caller must ensure its safe to read `environ` global value
    #[inline]
    unsafe fn environ() -> *mut *const *const libc::c_char {
        extern "C" {
            static mut environ: *const *const libc::c_char;
        }
        std::ptr::addr_of_mut!(environ)
    }

    /// # Safety
    ///
    /// caller must ensure its safe to read `environ` global value
    pub unsafe fn as_clist<'a>() -> CListMutPtr<'a> {
        CListMutPtr::from_raw_parts(*environ() as *mut *const libc::c_char)
    }

    /// # Safety
    ///
    /// caller must ensure its safe to read and write to `environ` global value
    /// returned pointer validity depends on the data pointed to by environ global variable
    pub unsafe fn swap(new: *const *const libc::c_char) -> *const *const libc::c_char {
        let old = *environ();
        *environ() = new;
        old
    }
}

pub struct Symbol<T> {
    ptr: *mut libc::c_void,
    phantom: std::marker::PhantomData<T>,
}

/// # Safety
///
/// caller must ensure that the symbol name reflects will resolve to supplied type
pub unsafe fn dlsym<T>(handle: *mut libc::c_void, str: &CStr) -> Option<Symbol<T>> {
    let ptr = libc::dlsym(handle, str.as_ptr());
    if ptr.is_null() {
        return None;
    }
    Some(Symbol {
        ptr,
        phantom: std::marker::PhantomData,
    })
}

impl<T> ::std::ops::Deref for Symbol<T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*(&self.ptr as *const *mut _ as *const T) }
    }
}

/// returns the path of the library from which the symbol pointed to by *addr* was loaded from
///
/// # Safety
/// addr must be a valid address accepted by dladdr(2)
pub unsafe fn get_dl_path_raw(addr: *const libc::c_void) -> (Option<CString>, Option<CString>) {
    let mut info = libc::Dl_info {
        dli_fname: ptr::null(),
        dli_fbase: ptr::null_mut(),
        dli_sname: ptr::null(),
        dli_saddr: ptr::null_mut(),
    };
    let res = libc::dladdr(addr, &mut info as *mut libc::Dl_info);

    if res == 0 {
        return (None, None);
    }
    let path_name = if info.dli_fbase.is_null() || info.dli_fname.is_null() {
        None
    } else {
        Some(CStr::from_ptr(info.dli_fname).to_owned())
    };

    let symbol_name = if info.dli_saddr.is_null() || info.dli_sname.is_null() {
        None
    } else {
        Some(CStr::from_ptr(info.dli_sname).to_owned())
    };

    (path_name, symbol_name)
}

/// On Stack vector that holds data that can be passed to C functions
/// that need a null terminated list of null terminated strings
///
/// example uses: execve fn
///
/// Additionally it can store CString values to facilitate keeping
/// heap allocated strings alongside a purely referential struct
pub struct ExecVec<const N: usize> {
    heap_items: SmallVec<[CString; 0]>,
    // Always NULL ptr terminated
    ptrs: SmallVec<[*const libc::c_char; N]>,
}

impl<const N: usize> ExecVec<N> {
    pub fn as_ptr(&self) -> *const *const libc::c_char {
        self.ptrs.as_ptr()
    }

    pub fn as_mut_ptr(&mut self) -> *mut *const libc::c_char {
        self.ptrs.as_mut_ptr()
    }

    pub fn empty() -> Self {
        let mut ptrs = SmallVec::new();
        ptrs.push(std::ptr::null());
        Self {
            heap_items: SmallVec::new(),
            ptrs,
        }
    }

    /// # Safety
    ///
    /// caller must ensure that all data pointed by cstrs will live
    /// for as long as ExecVec will live
    pub unsafe fn from_slice(src: &[&CStr]) -> Self {
        let mut ptrs = SmallVec::new();
        for str in src {
            ptrs.push(str.as_ptr());
        }
        ptrs.push(std::ptr::null());

        ExecVec {
            heap_items: SmallVec::new(),
            ptrs,
        }
    }

    /// # Safety
    ///
    /// caller must ensure the cstr will live for as long as ExecVec is valid
    pub unsafe fn push_cstr(&mut self, item: &CStr) {
        self.push_ptr(item.as_ptr());
    }

    /// # Safety
    ///
    /// caller must ensure the cstr will live for as long as ExecVec is valid
    pub unsafe fn insert_cstr(&mut self, pos: usize, item: &CStr) {
        self.insert_ptr(pos, item.as_ptr());
    }

    pub fn push_cstring(&mut self, item: CString) {
        let ptr = item.as_ptr();
        self.heap_items.push(item);
        unsafe { self.push_ptr(ptr) };
    }
    pub fn insert_cstring(&mut self, pos: usize, item: CString) {
        let ptr = item.as_ptr();
        self.heap_items.push(item);
        unsafe { self.insert_ptr(pos, ptr) };
    }

    /// # Safety
    ///
    /// caller must ensure the ptr will live for as long as ExecVec is valid
    pub unsafe fn push_ptr(&mut self, ptr: *const libc::c_char) {
        // replace previous trailing null with ptr to the item

        // check for additional nulls in the array - in case the
        // ptr array was modified out of bounds
        let number_of_strings = self.len();

        // ensure array has enough space for new ptr and the trailing null
        self.ptrs.resize(number_of_strings + 2, std::ptr::null());
        // append the pointer
        self.ptrs[number_of_strings] = ptr;
    }

    /// # Safety
    ///
    /// caller must ensure the ptr will live for as long as ExecVec is valid
    pub unsafe fn insert_ptr(&mut self, pos: usize, ptr: *const libc::c_char) {
        // replace previous trailing null with ptr to the item

        // check for additional nulls in the array - in case the
        // ptr array was modified out of bounds
        let number_of_strings = self.len();

        // ensure array has enough space for new ptr and the trailing null
        self.ptrs.resize(number_of_strings + 2, std::ptr::null());
        // insert the pointer
        self.ptrs.insert(pos, ptr);
    }

    /// number of string pointers in the clist
    pub fn len(&self) -> usize {
        let mut actual_length = self.ptrs.len();
        for i in (0..actual_length).rev() {
            if self.ptrs[i] == std::ptr::null() {
                actual_length = i;
            }
        }
        actual_length
    }

    pub fn as_clist<'a>(&'a mut self) -> CListMutPtr<'a> {
        unsafe { CListMutPtr::from_raw_parts(self.as_mut_ptr()) }
    }
}

pub struct CListMutPtr<'a> {
    inner: &'a mut [*const libc::c_char],
    elements: usize,
}

impl<'a> CListMutPtr<'a> {
    /// # Safety
    ///
    /// pointers passed to this method must remain valid for the lifetime of CListMutPtr object
    pub unsafe fn from_raw_parts(ptr: *mut *const libc::c_char) -> Self {
        let mut len = 0;
        while !(*ptr.add(len)).is_null() {
            len += 1;
        }
        Self {
            inner: std::slice::from_raw_parts_mut(ptr, len + 1),
            elements: len,
        }
    }

    pub fn as_ptr(&self) -> *const *const libc::c_char {
        self.inner.as_ptr()
    }

    /// Copies data into owned Vec<CString>
    ///
    /// # Safety
    ///
    /// caller must ensure the underlying pointer is safe to read and points to valid null teminated list
    /// of c strings
    pub unsafe fn to_owned_vec(&self) -> Vec<CString> {
        let mut vec = Vec::with_capacity(self.elements);
        for i in 0..self.elements {
            let elem = CStr::from_ptr(self.inner[i]);
            vec.push(elem.to_owned());
        }

        vec
    }

    /// remove entry from a slice, shifting other entries in its place
    ///
    /// # Safety
    /// entries in self.inner must be valid null terminated c strings
    pub unsafe fn remove_entry<M: EntryByteMatcher>(&mut self, matcher: M) -> Option<&CStr> {
        for i in (0..self.elements).rev() {
            let elem = CStr::from_ptr(self.inner[i]);
            if matcher.matches_bytes(elem.to_bytes()) {
                for src in i + 1..self.elements {
                    self.inner[src - 1] = self.inner[src]
                }
                self.elements -= 1;
                self.inner[self.elements] = std::ptr::null();

                return Some(elem);
            }
        }

        None
    }

    /// get entry from a slice
    ///
    /// # Safety
    /// entries in self.inner must be valid null terminated c strings
    pub unsafe fn get_entry<M: EntryByteMatcher>(&mut self, matcher: M) -> Option<&CStr> {
        for i in (0..self.elements).rev() {
            let elem = CStr::from_ptr(self.inner[i]);
            if matcher.matches_bytes(elem.to_bytes()) {
                return Some(elem);
            }
        }

        None
    }

    /// get entry from a slice
    ///
    /// # Safety
    /// entries in self.inner must be valid null terminated c strings
    pub unsafe fn get_first_entry<M: EntryByteMatcher>(&mut self, matcher: M) -> Option<&CStr> {
        if self.elements > 0 {
            let elem = CStr::from_ptr(self.inner[0]);
            if matcher.matches_bytes(elem.to_bytes()) {
                return Some(elem);
            }  
        }

        None
    }

    /// replace entry in a slice
    ///
    /// # Safety
    ///
    /// entries in self.inner must be valid null terminated c strings
    ///
    /// new_entry must live as long as CListMutPtr a
    pub unsafe fn replace_entry<F: Fn(&[u8]) -> bool>(
        &mut self,
        predicate: F,
        new_entry: &CStr,
    ) -> Option<*const libc::c_char> {
        for i in 0..self.elements {
            let elem = CStr::from_ptr(self.inner[i]);
            if predicate(elem.to_bytes()) {
                self.inner[i] = new_entry.as_ptr();
                return Some(elem.as_ptr());
            }
        }
        None
    }

    pub fn len(&self) -> usize {
        self.elements
    }

    /// create exec vec with size N allocated on stack, and copy all pointer there,
    /// if there are more entries than N - ExecVec will allocate more space on heap
    ///
    /// # Safety
    ///
    /// entries in self.inner must be valid null terminated c strings valid for the lifetime of ExecVec
    ///
    /// if there are more elements in self.inner than N - caller must ensure the context allows safe heap allocations
    pub unsafe fn into_exec_vec<const N: usize>(self) -> ExecVec<N> {
        let mut vec: ExecVec<N> = ExecVec::empty();
        for i in 0..self.elements {
            vec.push_ptr(self.inner[i]);
        }
        vec
    }
}

pub mod ext {
    use std::ffi::CStr;

    pub trait CStrExt {
        /// Splits CStr into two parts based on supplied character
        ///
        /// it will not do any additional allcoation, and will reuse the memory
        /// pointed to by CStr
        ///
        /// first part, preceeding the character is returned as slice
        ///
        /// second part can be returned as valid &CStr because
        /// it will be properly null terminated
        fn split_once(&self, c: u8) -> (&[u8], &CStr);
    }
    impl CStrExt for CStr {
        fn split_once(&self, c: u8) -> (&[u8], &CStr) {
            let bytes = self.to_bytes_with_nul();
            for i in 0..bytes.len() - 1 {
                if bytes[i] == c {
                    // safety: we're processing existing CStr so its guaranteed to be null terminated
                    return (&bytes[0..i], unsafe {
                        CStr::from_bytes_with_nul_unchecked(&bytes[i + 1..])
                    });
                }
            }
            (&[], self)
        }
    }
}

pub trait EntryByteMatcher {
    fn matches_bytes(&self, data: &[u8]) -> bool;
}

pub struct Exact<'a>(&'a [u8]);

impl<'a> EntryByteMatcher for Exact<'a> {
    fn matches_bytes(&self, data: &[u8]) -> bool {
        self.0 == data
    }
}

impl<'a> Exact<'a> {
    pub const fn from(str: &'a str) -> Self {
        Exact(str.as_bytes())
    }
}

pub struct StartsWith<'a>(&'a [u8]);

impl<'a> EntryByteMatcher for StartsWith<'a> {
    fn matches_bytes(&self, data: &[u8]) -> bool {
        data.starts_with(self.0)
    }
}

impl<'a> StartsWith<'a> {
    pub const fn from(str: &'a str) -> Self {
        StartsWith(str.as_bytes())
    }
}

pub struct EndsWith<'a>(&'a [u8]);

impl<'a> EntryByteMatcher for EndsWith<'a> {
    fn matches_bytes(&self, data: &[u8]) -> bool {
        data.ends_with(self.0)
    }
}

impl<'a> EndsWith<'a> {
    pub const fn from(str: &'a str) -> Self {
        EndsWith(str.as_bytes())
    }
}

pub struct EnvKey<'a>(&'a [u8]);

impl<'a> EnvKey<'a> {
    pub const fn from(str: &'a str) -> Self {
        EnvKey(str.as_bytes())
    }

    pub fn get_value(src: &CStr) -> &CStr {
        let (_, val) = src.split_once(b'=');
        val
    }

    pub fn build_c_env<S: AsRef<str>>(&self, val: S) -> anyhow::Result<CString> {
        let str = String::from_utf8(self.0.to_vec())? + "=" + val.as_ref();

        Ok(CString::new(str)?)
    }
}

impl<'a> From<&'a CStr> for EnvKey<'a> {
    fn from(str: &'a CStr) -> Self {
        EnvKey(str.to_bytes())
    }
}

impl<'a> From<&'a str> for EnvKey<'a> {
    fn from(str: &'a str) -> Self {
        EnvKey(str.as_bytes())
    }
}

impl<'a> EntryByteMatcher for EnvKey<'a> {
    fn matches_bytes(&self, data: &[u8]) -> bool {
        let key = self.0;
        // data must be key.len + 1 char long at least as it must also hold '=' char
        if data.len() <= key.len() {
            return false;
        }

        if !data.starts_with(key) {
            return false;
        }

        // key found in start of data, now it should be followed by '=' char
        data[key.len()] == b'='
    }
}

#[cfg(test)]
mod tests {
    use ddcommon::cstr;

    use super::{ext::CStrExt, CListMutPtr, EnvKey, ExecVec};

    #[test]
    fn test_clist_element_fetch_and_removal() {
        let mut vec = ExecVec::<10>::empty();
        unsafe { vec.push_cstr(cstr!("hello")) };
        unsafe { vec.push_cstr(cstr!("ehlo=")) };
        unsafe { vec.push_cstr(cstr!("1")) };
        unsafe { vec.push_cstr(cstr!("2")) };
        assert_eq!(4, vec.len());

        let mut clist: CListMutPtr = vec.as_clist();
        // fetch entry from CList
        assert_eq!(
            cstr!("ehlo="),
            unsafe { clist.get_entry(EnvKey::from("ehlo")) }.unwrap()
        );
        // remove entry from CList
        assert_eq!(
            cstr!("ehlo="),
            unsafe { clist.remove_entry(EnvKey::from("ehlo")) }.unwrap()
        );
        assert_eq!(3, clist.len());
        // underlying vec should be updated as well
        assert_eq!(3, vec.as_clist().len());
        assert_eq!(3, vec.len());
        unsafe {
            assert_eq!(
                vec![
                    cstr!("hello").to_owned(),
                    cstr!("1").to_owned(),
                    cstr!("2").to_owned()
                ],
                vec.as_clist().to_owned_vec()
            )
        };
    }
    #[test]
    fn test_exec_vec_cstr_lifetime() {
        let str = cstr!("a string").to_owned();
        let mut vec = ExecVec::<10>::empty();
        unsafe { vec.push_cstr(str.as_c_str()) };
        // CString must not be dropped now

        unsafe {
            assert_eq!(
                vec![cstr!("a string").to_owned()],
                vec.as_clist().to_owned_vec()
            )
        };
    }

    #[test]
    fn test_split_cstr() {
        let (bytes, str) = cstr!("string_a=string_b").split_once(b'=');
        assert_eq!(b"string_a", bytes);
        assert_eq!(cstr!("string_b"), str);
    }

    #[test]
    fn test_exec_vec_insert_ptr() {
        let mut vec = ExecVec::<10>::empty();
        unsafe { vec.push_cstr(cstr!("3")) };
        unsafe { vec.insert_cstr(0, cstr!("1")) };
        vec.insert_cstring(1, cstr!("2").to_owned());

        unsafe {
            assert_eq!(
                vec![
                    cstr!("1").to_owned(),
                    cstr!("2").to_owned(),
                    cstr!("3").to_owned()
                ],
                vec.as_clist().to_owned_vec()
            )
        };
    }
}

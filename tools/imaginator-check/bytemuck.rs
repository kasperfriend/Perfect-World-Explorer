//! Offline stand-in for `bytemuck`: exactly the items the Imaginator uses.
//! Harness for `tools/imaginator-check.sh`, not shipped.
#![allow(dead_code)]

pub unsafe trait Pod: Copy + 'static {}
unsafe impl Pod for f32 {}
unsafe impl Pod for u32 {}
unsafe impl Pod for u16 {}
unsafe impl Pod for u8 {}
unsafe impl Pod for i32 {}

pub unsafe trait Zeroable: Sized {
    fn zeroed() -> Self {
        unsafe { std::mem::zeroed() }
    }
}
unsafe impl Zeroable for f32 {}
unsafe impl Zeroable for u32 {}
unsafe impl Zeroable for u16 {}
unsafe impl Zeroable for u8 {}
unsafe impl Zeroable for i32 {}

pub fn bytes_of<T: Pod>(value: &T) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(value as *const T as *const u8, std::mem::size_of::<T>())
    }
}

pub fn cast_slice<A: Pod, B: Pod>(values: &[A]) -> &[B] {
    let bytes = std::mem::size_of_val(values);
    assert_eq!(bytes % std::mem::size_of::<B>(), 0);
    if bytes == 0 {
        return &[];
    }
    unsafe {
        std::slice::from_raw_parts(values.as_ptr() as *const B, bytes / std::mem::size_of::<B>())
    }
}

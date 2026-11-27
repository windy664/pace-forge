//! 数字类型转换：每一处都把「坏值怎么办」写在名字或返回值上。
//!
//! 约定：
//! - **来自外部数据的数**（用户提交的 JSON、服务端响应）一律走这里的
//!   `*_to_*` 检查函数，拿到 `Option` 后由调用方决定兜底值或报错。
//! - `now_ms()` 直接返回 [`i64`]：毫秒时间戳不可能为负，也不需要无符号。
//! - 像素与几何计算里保留 `as`：那里「向零截断、越界饱和」正是想要的行为，
//!   换成会失败的转换只会把控制流搅乱。新增外部数据的转换不要再用 `as`。

/// `f64` → [`i64`]：`None` 表示 NaN、无穷或超出范围。
pub fn f64_to_i64(x: f64) -> Option<i64> {
    // `as` saturates and maps NaN to 0, so a round trip proves the value survived.
    let n = x as i64;
    (n as f64 == x && x.is_finite()).then_some(n)
}

/// `f64` → [`i64`]：向零截断（原来写成 `as i64` 的地方用它），NaN 或越界是 `None`。
pub fn trunc_to_i64(x: f64) -> Option<i64> {
    (x.is_finite() && (-9_007_199_254_740_992.0..9_007_199_254_740_992.0).contains(&x))
        .then_some(x as i64)
}

/// 先四舍五入再转 [`i64`]；`None` 的含义同 [`f64_to_i64`]。
pub fn round_to_i64(x: f64) -> Option<i64> {
    f64_to_i64(x.round())
}

/// `f64` → [`usize`]：负数、NaN、小数部分都是 `None`（不悄悄截断）。
pub fn f64_to_usize(x: f64) -> Option<usize> {
    let n = f64_to_i64(x)?;
    usize::try_from(n).ok()
}

/// `i64` → [`i32`]：超出范围是 `None`。
pub fn i64_to_i32(x: i64) -> Option<i32> {
    i32::try_from(x).ok()
}

/// `i64` → [`u8`]：负数或大于 255 是 `None`。
pub fn i64_to_u8(x: i64) -> Option<u8> {
    u8::try_from(x).ok()
}

/// `i64` → [`u32`]：负数是 `None`。
pub fn i64_to_u32(x: i64) -> Option<u32> {
    u32::try_from(x).ok()
}

/// `i64` → [`u64`]：负数是 `None`。
pub fn i64_to_u64(x: i64) -> Option<u64> {
    u64::try_from(x).ok()
}

/// `i64` → [`usize`]：负数是 `None`。
pub fn i64_to_usize(x: i64) -> Option<usize> {
    usize::try_from(x).ok()
}

/// `u128`（例如 `Duration::as_millis()`）→ [`i64`]，超过 [`i64::MAX`] 就钉在上限。
///
/// 用于「只可能因为时钟离谱才溢出」的地方：饱和比失败更合适。
pub fn u128_to_i64_saturating(x: u128) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

/// `u128` → [`u64`]，超过 [`u64::MAX`] 就钉在上限（例如 `Duration::as_millis()`）。
pub fn u128_to_u64_saturating(x: u128) -> u64 {
    u64::try_from(x).unwrap_or(u64::MAX)
}

/// `u64` → [`i64`]：超过 [`i64::MAX`] 是 `None`。
pub fn u64_to_i64(x: u64) -> Option<i64> {
    i64::try_from(x).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_to_i64_rejects_non_finite_and_out_of_range() {
        assert_eq!(f64_to_i64(12.0), Some(12));
        assert_eq!(f64_to_i64(-3.0), Some(-3));
        assert_eq!(f64_to_i64(f64::NAN), None);
        assert_eq!(f64_to_i64(f64::INFINITY), None);
        assert_eq!(f64_to_i64(1e30), None);
    }

    #[test]
    fn float_to_i64_keeps_large_integral_values() {
        // A millisecond timestamp is far inside the exact range of f64.
        assert_eq!(f64_to_i64(1_790_913_882_164.0), Some(1_790_913_882_164));
    }

    #[test]
    fn float_to_usize_refuses_to_truncate() {
        assert_eq!(f64_to_usize(2.0), Some(2));
        assert_eq!(f64_to_usize(2.5), None);
        assert_eq!(f64_to_usize(-1.0), None);
    }

    #[test]
    fn round_then_convert() {
        assert_eq!(round_to_i64(12.5), Some(13));
        assert_eq!(round_to_i64(-0.4), Some(0));
        assert_eq!(round_to_i64(f64::NAN), None);
    }

    #[test]
    fn narrowing_reports_out_of_range() {
        assert_eq!(i64_to_i32(i64::from(i32::MAX)), Some(i32::MAX));
        assert_eq!(i64_to_i32(i64::from(i32::MAX) + 1), None);
        assert_eq!(i64_to_u8(8), Some(8));
        assert_eq!(i64_to_u8(-1), None);
        assert_eq!(i64_to_u32(-1), None);
        assert_eq!(i64_to_u64(-1), None);
        assert_eq!(i64_to_usize(3), Some(3));
        assert_eq!(u64_to_i64(u64::MAX), None);
    }
}

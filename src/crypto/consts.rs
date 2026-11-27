//! NetSecKit 4.2.4 协议常量。
//!
//! 数值经实机报文验证；**出处不是 `libnetseckit-4.2.4.so`**——7.4.20 的
//! 逆向（2026-11-23）把该 so 的 .text 全量反汇编后，三段 KDF 的 `ror #8/#24`、
//! MD5 K 表/IV、RSA、盐常量全部 0 命中，它只是易盾 NetSecKit 的通用密码学
//! JNI 后端（SM2 + LibTomMath + rijndael）。这些常量的真实来源未定位，
//! 现有值是「能对上服务器」的经验值，按【实机联调】这一级引用。
//! 每一处改动都必须重新跑 `cargo test` 并用 `selftest` 对真实服务器确认。

/// 请求签名盐。跨版本通用（7.3.11 → 7.4.01 未变）。
///
/// 出处可以再收一格：7.4.20 的 `NativeNetworkGatewaySign.salt()` 读的就是
/// `SportJniUtils.getSecretKey(5)`，失败才回落到 `R.string.md5_sign_salt`；
/// `SportJniUtils` 的 `<clinit>` 是 `System.loadLibrary("swsport")`，所以这个
/// 盐是 **`libswsport.so` 里的常量**，既不在 dex 字符串表里（Inverx FridaDump
/// 8 个 dex 全量 0 命中），也不在 `libnetseckit-4.2.4.so` 里。
/// 见 `docs/notes/FRIDADUMP-7420-CHECK.md`。
pub const SIGN_SALT: &str = "2slhe02lsfiwowlcixisla_sls-_slaor";

/// 请求 RSA 公钥（PKCS#1，1024-bit DER，base64）。
/// 用途：加密每次请求随机生成的 AES-128 key。
pub const REQ_RSA_PKCS1_B64: &str = concat!(
    "MIGJAoGBALmmqVPOwY1mTVHGEfg7jHck1MNXsVtUfrqY99bm/W2cjLi3LvG/wMYw",
    "bhmf9O+y3CUlZ5g4AMd2Ly4XbWUloG/O8+1USGJ8ddmrzbl8j2EZi0OhpVCYI381",
    "+zZ0qa3z1cOuG2XdJnE7pO6Y4JNC37miqMXYmjejVi2QDIbONomfAgMBAAE="
);

/// 响应验签公钥（SPKI，1024-bit DER，base64）。
/// 用途：验证服务端对响应明文的 PKCS#1 v1.5 签名。
pub const RESP_VERIFY_SPKI_B64: &str = concat!(
    "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDMs6bFhb5lrf/xqwtNcKVHW09c",
    "oMbtLAmr9Rti7vnSnVroFuLHJEzxMqPQeItByr1Fu4RAQMR/aq/oknn53kCOuSaV",
    "sd9mnDCusrs7GmExDxlKe2pNAKTEZnT1FITQYT2SJkeCZ8iBKujqN61C3SXQ4Xa8",
    "QRr2uMTicec6XC08WQIDAQAB"
);

/// 每次请求 AES key 的字符集。
pub const KEY_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// keyData 四段拼接用的固定尾巴。
pub const KEYDATA_TAIL: &str = "ABCDEFGHIJKLMN12";

/// 请求信封的协议版本标识。
pub const PROTO_VERSION: &str = "101";

/// 业务成功码。
pub const CODE_OK: i64 = 10000;

/// 服务端内部故障。
///
/// 这个码不是「业务拒绝」，是后端的异常透传：服务之间的调用被注册中心
/// 白名单挡掉时，网关会把对方的异常信息原样塞进 `message`，典型内容是
/// `Forbid consumer 10.x.x.x access service com.iydsj.server.basicuser.service
/// .UserBasicService from registry 10.0.30.34:2181 ... whitelist/blacklist`。
///
/// 一旦出现，登录、查记录、拉策略全都会中招，且与账号、密码、设备指纹、
/// 网络、客户端版本**完全无关**。区别对待它，用户才不会去重装、换号、
/// 挂代理白折腾一轮（issue #3 里有人折腾了两天）。
pub const CODE_SERVER_FAULT: i64 = -10000;

/// 网关主机。
pub mod hosts {
    /// 主业务网关。
    pub const RUN: &str = "https://run.gxapp.iydsj.com";
    /// 服务发现网关。
    pub const DISCOVERY: &str = "https://discovery.gxapp.iydsj.com";
    /// 埋点专用。注意 `startup/pv` 只在这个 host 上存在，run 会返回 404。
    pub const DATAPOINT: &str = "https://datapoint.gxapp.iydsj.com";
    /// 合作方接口网关。
    pub const PARTNER: &str = "https://partner.iydsj.com";
}

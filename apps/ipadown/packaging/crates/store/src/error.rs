use thiserror::Error;

#[derive(Error, Debug)]
pub enum StoreError {
    #[error("网络请求失败: {0}")]
    Http(#[from] reqwest::Error),

    #[error("plist 解析失败: {0}")]
    Plist(String),

    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),

    #[error("zip 处理失败: {0}")]
    Zip(#[from] zip::result::ZipError),

    #[error("JSON 错误: {0}")]
    Json(#[from] serde_json::Error),

    #[error("需要双重验证码（2FA）")]
    CodeRequired,

    #[error("登录失败: {0}")]
    AuthFailed(String),

    #[error("响应格式不符合预期: {0}")]
    UnexpectedResponse(String),

    /// 上游服务侧故障/能力限制，内容本身已是面向用户的完整提示，展示时不再加前缀
    #[error("{0}")]
    Upstream(String),

    #[error("未找到登录账号，请先登录 Apple ID")]
    NoAccount,

    #[error("配置错误: {0}")]
    Config(String),
}

impl From<plist::Error> for StoreError {
    fn from(e: plist::Error) -> Self {
        StoreError::Plist(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

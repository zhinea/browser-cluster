use thiserror::Error;

#[derive(Error, Debug)]
pub enum BrowserError {
    #[error("Browser launch failed: {0}")]
    LaunchError(String),
    #[error("Browser operation failed: {0}")]
    OperationError(String),
    #[error("Browser connection failed: {0}")]
    ConnectionError(String),
    #[error("Timeout error: {0}")]
    TimeoutError(String),
    #[error("Queue is full")]
    QueueFullError,
}


pub trait Browser: Send + Sync + 'static {
    async fn new() -> Result<Self, BrowserError> where Self: Sized;
    async fn close(&self) -> Result<(), BrowserError>;
}

pub enum QueueMode {
    FIFO, // First in first out
    LIFO // Last in first out
}
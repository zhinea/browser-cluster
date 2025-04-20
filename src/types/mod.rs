use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use async_trait::async_trait;
use futures::future::BoxFuture;
use thiserror::Error;
use tokio::{

};
use tokio::sync::{mpsc, Mutex, Semaphore};

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

#[async_trait]
pub trait Task<B, T>
where
    B: Browser,
    T: Send + 'static,
{
    async fn execute(&self, browser: Arc<B>, data: T) -> Result<(), BrowserError>;
}

pub struct WorkerOptions {
    pub idle_timeout: Option<Duration>,
    pub max_browser_lifetime: Option<Duration>,
}

pub struct Worker<B> {
    pub browser: Arc<B>,
    pub created_at: Instant,
    pub last_used: Instant,
}

pub struct ClusterOptions {
    pub concurrency: usize,
    pub max_concurrent_browsers: usize,
    pub min_idle_browsers: usize,
    pub max_queue_size: Option<usize>,
    pub idle_timeout: Option<Duration>,
    pub max_browser_lifetime: Option<Duration>,
    pub retry_limit: usize,
    pub retry_delay: Duration,
    pub queue_mode: QueueMode,
    pub monitor_interval: Duration,
}

pub type TaskFn<B, T>= Box<dyn Fn(Arc<B>, T) -> BoxFuture<'static, Result<(), BrowserError>> + Send + Sync>;

pub struct JobEntry<B, T>
where
    B: Browser,
    T: Send + 'static,
{
    pub task: Arc<TaskFn<B, T>>,
    pub data: T,
    pub retries: usize
}

pub struct Cluster<B, T>
where
    B: Browser,
    T: Send + 'static
{
    pub options: ClusterOptions,
    pub active_workers: Arc<Mutex<Vec<Worker<B>>>>,
    pub idle_workers: Arc<Mutex<VecDeque<Worker<B>>>>,
    pub job_queue: Arc<Mutex<VecDeque<JobEntry<B, T>>>>,
    pub concurrency_semaphore: Arc<Semaphore>,
    pub worker_count: Arc<Mutex<usize>>,
    pub is_closed: Arc<Mutex<bool>>,
    pub task_channel: (mpsc::Sender<JobEntry<B, T>>, Arc<Mutex<mpsc::Receiver<JobEntry<B, T>>>>)
}

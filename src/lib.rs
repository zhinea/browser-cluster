use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::{
    sync::{Mutex}
};
use tokio::sync::Semaphore;
use tokio::time::sleep;
use crate::types::{Browser, BrowserError, Cluster, ClusterOptions, JobEntry, QueueMode, TaskFn, Worker, WorkerOptions};

mod types;

impl <B: Browser>Worker<B> {

    pub async fn new() -> Result<Self, BrowserError> {
        let browser = Arc::new(B::new().await?);
        let now = Instant::now();

        Ok(Self {
            browser,
            created_at: now,
            last_used_at: now,
        })
    }

    pub fn should_replace(&self, options: &WorkerOptions) -> bool {
        let now = Instant::now();

        if let Some(max_lifetime) = options.max_browser_lifetime {
            if now.duration_since(self.created_at) > max_lifetime {
                return true;
            }
        }

        if let Some(max_idle_time) = options.idle_timeout {
            if now.duration_since(self.last_used_at) > max_idle_time {
                return true;
            }
        }

        false
    }
}

impl Default for ClusterOptions {
    fn default() -> Self {
        Self {
            concurrency: num_cpus::get(),
            max_concurrent_browsers: num_cpus::get(),
            min_idle_browsers: 0,
            max_queue_size: Some(100),
            idle_timeout: Some(Duration::from_secs(30 * 60)), // 30 minutes
            max_browser_lifetime: Some(Duration::from_secs(3 * 60 * 60)), // 3 hours
            retry_limit: 3,
            retry_delay: Duration::from_secs(1),
            queue_mode: QueueMode::FIFO,
            monitor_interval: Duration::from_secs(30),
        }
    }
}

impl <B, T> Cluster<B, T>
where
    B: Browser,
    T: Send + 'static
{

    pub async fn new(options: ClusterOptions) -> Result<Self, BrowserError> {
        let active_workers = Arc::new(Mutex::new(Vec::new()));
        let idle_workers = Arc::new(Mutex::new(VecDeque::new()));
        let job_queue = Arc::new(Mutex::new(VecDeque::new()));
        let concurrency_semaphore = Arc::new(Semaphore::new(options.concurrency));
        let worker_count = Arc::new(Mutex::new(0));
        let is_closed = Arc::new(Mutex::new(false));
        let (tx, rx) = tokio::sync::mpsc::channel(options.concurrency * 2);
        let rx = Arc::new(Mutex::new(rx));

        let cluster = Self{
            options,
            active_workers,
            idle_workers,
            job_queue,
            concurrency_semaphore,
            worker_count,
            is_closed,
            task_channel: (tx, rx)
        };

        // Initialize idle browsers if needed
        cluster.ensure_min_idle_browsers().await?;

        // Start the monitoring task
        cluster.start_monitor();

        // Start the task processor
        cluster.start_task_processor();

        Ok(cluster)
    }

    async fn ensure_min_idle_browsers(&self) -> Result<(), BrowserError> {
        let current_idle = self.idle_workers.lock().await.len();
        let current_total = {
            let mut count = *self.worker_count.lock().await;
            count
        };

        let to_create = (self.options.min_idle_browsers as isize - current_idle as isize)
            .max(0) as usize;
        let max_new = (self.options.max_concurrent_browsers - current_total)
            .max(0);

        let to_create = to_create.min(max_new);

        let mut worker_count = self.worker_count.lock().await;

        // create browser in parallel
        let mut handles = Vec::new();
        for _ in 0..to_create {
            *worker_count += 1;
            handles.push(Worker::new());
        }

        let results = futures::future::join_all(handles).await;

        let mut idle_workers = self.idle_workers.lock().await;
        for result in results {
            match result {
                Ok(worker) => {
                    idle_workers.push(worker)
                },
                Err(err) => {
                    *worker_count -= 1;
                    return Err(err);
                }
            }
        }

        Ok(())
    }

    pub async fn execute<F, Fut>(
        &self,
        task: F,
        data: T
    ) -> Result<(), BrowserError>
    where
        F: Fn(Arc<B>, T) -> Fut + Send + Sync + 'static,
        Fut: futures::Future<Output = Result<(), BrowserError>> + Send + 'static,
    {
        if *self.is_closed.lock().await {
            return Err(BrowserError::OperationError("Cluster is closed".to_string()));
        }

        if let Some(max_size) = self.options.max_queue_size {
            if self.job_queue.lock().await.len() >= max_size {
                return Err(BrowserError::QueueFullError);
            }
        }

        let task_fn: TaskFn<B, T> = Box::new(move |browser, data| {
            Box::pin(task(browser, data))
        });

        let job = JobEntry {
            task: Arc::new(task_fn),
            data,
            retries: 0
        };

        self.task_channel.0.send(job).await
            .map_err(|_| BrowserError::OperationError("Failed to send task".to_string()))?;

        Ok(())
    }
    fn start_task_processor(&self) {
        let options = self.options.clone();
        let active_workers = self.active_workers.clone();
        let idle_workers = self.idle_workers.clone();
        let job_queue = self.job_queue.clone();
        let concurrency_semaphore = self.concurrency_semaphore.clone();
        let worker_count = self.worker_count.clone();
        let is_closed = self.is_closed.clone();
        let rx = self.task_channel.1.clone();

        tokio::spawn(async move {
            let mut receiver = rx.lock().await;

            while let Some(job) = receiver.recv().await {
                if *is_closed.lock().await {
                    break;
                }

                // Queue the job
                {
                    let mut queue = job_queue.lock().await;
                    match options.queue_mode {
                        QueueMode::FIFO => queue.push_back(job),
                        QueueMode::LIFO => queue.push_front(job),
                    }
                }

                // Try to process jobs
                Self::process_queue(
                    &options,
                    &active_workers,
                    &idle_workers,
                    &job_queue,
                    &concurrency_semaphore,
                    &worker_count,
                    &is_closed,
                ).await;
            }
        });
    }

    async fn process_queue(
        options: &ClusterOptions,
        active_workers: &Arc<Mutex<Vec<Worker<B>>>>,
        idle_workers: &Arc<Mutex<VecDeque<Worker<B>>>>,
        job_queue: &Arc<Mutex<VecDeque<JobEntry<B, T>>>>,
        concurrency_semaphore: &Arc<Semaphore>,
        worker_count: &Arc<Mutex<usize>>,
        is_closed: &Arc<Mutex<bool>>,
    ) {
        // Process as many jobs as possible
        loop {
            // Check if closed
            if *is_closed.lock().await {
                break;
            }

            // Check if there are jobs
            let job = {
                let mut queue = job_queue.lock().await;
                if queue.is_empty() {
                    break;
                }
                queue.pop_front()
            };

            let Some(job) = job else { break };

            // Acquire a permit from the semaphore
            let permit = match concurrency_semaphore.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => {
                    // No permits available, put job back in queue
                    let mut queue = job_queue.lock().await;
                    match options.queue_mode {
                        QueueMode::FIFO => queue.push_back(job),
                        QueueMode::LIFO => queue.push_front(job),
                    }
                    break;
                }
            };

            // Get or create a worker
            let worker_result = Self::get_or_create_worker(
                options,
                idle_workers,
                worker_count,
            ).await;

            let worker = match worker_result {
                Ok(worker) => worker,
                Err(e) => {
                    // Failed to get a worker, put job back in queue
                    let mut queue = job_queue.lock().await;
                    match options.queue_mode {
                        QueueMode::FIFO => queue.push_back(job),
                        QueueMode::LIFO => queue.push_front(job),
                    }

                    // Drop the permit to allow other tasks to run
                    drop(permit);

                    eprintln!("Failed to get worker: {:?}", e);
                    break;
                }
            };

            // Move worker to active
            {
                let mut active = active_workers.lock().await;
                active.push(worker);
            }

            // Get the last worker (the one we just added)
            let worker_idx = {
                let active = active_workers.lock().await;
                active.len() - 1
            };

            // Clone necessary components for the task
            let active_workers_clone = active_workers.clone();
            let idle_workers_clone = idle_workers.clone();
            let job_queue_clone = job_queue.clone();
            let options_clone = options.clone();
            let worker_count_clone = worker_count.clone();
            let is_closed_clone = is_closed.clone();

            // Execute the task in a separate task
            tokio::spawn(async move {
                let browser = {
                    let active = active_workers_clone.lock().await;
                    active[worker_idx].browser.clone()
                };

                let task_result = match (job.task)(browser, job.data).await {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        if job.retries < options_clone.retry_limit {
                            // Create new job with incremented retries
                            let new_job = JobEntry {
                                task: job.task,
                                data: e, // We'd need to implement Clone for T or handle differently
                                retries: job.retries + 1,
                            };

                            // Add back to queue
                            let mut queue = job_queue_clone.lock().await;
                            match options_clone.queue_mode {
                                QueueMode::FIFO => queue.push_back(new_job),
                                QueueMode::LIFO => queue.push_front(new_job),
                            }

                            // Wait before retrying
                            sleep(options_clone.retry_delay).await;

                            Err(e)
                        } else {
                            // Log error after max retries
                            eprintln!("Task failed after {} retries: {:?}",
                                      options_clone.retry_limit, e);
                            Err(e)
                        }
                    }
                };

                // Move the worker back to idle
                let mut active = active_workers_clone.lock().await;
                if worker_idx < active.len() {
                    let worker = active.remove(worker_idx);

                    // Update last used time
                    let mut worker = worker;
                    worker.last_used = Instant::now();

                    // Check if worker should be replaced
                    let worker_options = WorkerOptions {
                        idle_timeout: options_clone.idle_timeout,
                        max_browser_lifetime: options_clone.max_browser_lifetime,
                    };

                    if worker.should_replace(&worker_options) {
                        // Close the browser
                        let _ = worker.browser.close().await;

                        // Decrement worker count
                        let mut count = worker_count_clone.lock().await;
                        *count -= 1;

                        // Create a new worker if needed
                        if !*is_closed_clone.lock().await {
                            Self::ensure_min_idle_browsers_inner(
                                &options_clone,
                                &idle_workers_clone,
                                &worker_count_clone,
                            ).await.expect("Failed to ensure min idle browsers");
                        }
                    } else {
                        // Put back in idle pool
                        let mut idle = idle_workers_clone.lock().await;
                        idle.push_back(worker);
                    }
                }

                // Release permit
                drop(permit);

                // If task was successful, try to process more from queue
                if task_result.is_ok() {
                    Self::process_queue(
                        &options_clone,
                        &active_workers_clone,
                        &idle_workers_clone,
                        &job_queue_clone,
                        concurrency_semaphore,
                        &worker_count_clone,
                        &is_closed_clone,
                    ).await;
                }
            });
        }
    }

    async fn get_or_create_worker(
        options: &ClusterOptions,
        idle_workers: &Arc<Mutex<VecDeque<Worker<B>>>>,
        worker_count: &Arc<Mutex<usize>>,
    ) -> Result<Worker<B>, BrowserError> {
        // Try to get an idle worker
        {
            let mut idle = idle_workers.lock().await;
            if let Some(worker) = idle.pop_front() {
                return Ok(worker);
            }
        }

        // No idle workers, create a new one if possible
        {
            let mut count = worker_count.lock().await;
            if *count >= options.max_concurrent_browsers {
                return Err(BrowserError::OperationError(
                    "Max concurrent browsers reached".to_string()
                ));
            }

            *count += 1;
        }

        // Create a new worker
        match Worker::new().await {
            Ok(worker) => Ok(worker),
            Err(e) => {
                let mut count = worker_count.lock().await;
                *count -= 1;
                Err(e)
            }
        }
    }

    async fn ensure_min_idle_browsers_inner(
        options: &ClusterOptions,
        idle_workers: &Arc<Mutex<VecDeque<Worker<B>>>>,
        worker_count: &Arc<Mutex<usize>>,
    ) -> Result<(), BrowserError> {
        let current_idle = idle_workers.lock().await.len();
        let current_total = {
            let count = *worker_count.lock().await;
            count
        };

        let to_create = (options.min_idle_browsers as isize - current_idle as isize)
            .max(0) as usize;

        let max_new = (options.max_concurrent_browsers - current_total)
            .max(0);

        let to_create = to_create.min(max_new);

        let mut worker_count_guard = worker_count.lock().await;

        // Create browsers in parallel for efficiency
        let mut handles = Vec::new();
        for _ in 0..to_create {
            *worker_count_guard += 1;
            handles.push(Worker::new());
        }

        let results = futures::future::join_all(handles).await;

        let mut idle_workers_guard = idle_workers.lock().await;
        for result in results {
            match result {
                Ok(worker) => {
                    idle_workers_guard.push_back(worker);
                }
                Err(e) => {
                    *worker_count_guard -= 1;
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    fn start_monitor(&self) {
        let options = self.options.clone();
        let active_workers = self.active_workers.clone();
        let idle_workers = self.idle_workers.clone();
        let worker_count = self.worker_count.clone();
        let is_closed = self.is_closed.clone();

        tokio::spawn(async move {
            loop {
                sleep(options.monitor_interval).await;

                // Check if cluster is closed
                if *is_closed.lock().await {
                    break;
                }

                // Clean up idle workers
                {
                    let mut idle = idle_workers.lock().await;
                    let worker_options = WorkerOptions {
                        idle_timeout: options.idle_timeout,
                        max_browser_lifetime: options.max_browser_lifetime,
                    };

                    let mut i = 0;
                    while i < idle.len() {
                        if idle[i].should_replace(&worker_options) {
                            // Over idle timeout, remove and close
                            if let Some(worker) = idle.remove(i) {
                                let _ = worker.browser.close().await;

                                // Decrement worker count
                                let mut count = worker_count.lock().await;
                                *count -= 1;
                            }
                        } else {
                            i += 1;
                        }
                    }
                }

                // Ensure min idle browsers
                let _ = Self::ensure_min_idle_browsers_inner(
                    &options,
                    &idle_workers,
                    &worker_count,
                ).await;
            }
        });
    }

    pub async fn queue_size(&self) -> usize {
        self.job_queue.lock().await.len()
    }

    pub async fn idle_size(&self) -> usize {
        self.idle_workers.lock().await.len()
    }

    pub async fn active_size(&self) -> usize {
        self.active_workers.lock().await.len()
    }

    pub async fn worker_count(&self) -> usize {
        *self.worker_count.lock().await
    }

    pub async fn close(&self) -> Result<(), BrowserError> {
        // Mark as closed
        {
            let mut closed = self.is_closed.lock().await;
            *closed = true;
        }

        // Close all active browsers
        {
            let mut active = self.active_workers.lock().await;
            let close_futures: Vec<_> = active.drain(..)
                .map(|w| w.browser.close())
                .collect();

            let _ = futures::future::join_all(close_futures).await;
        }

        // Close all idle browsers
        {
            let mut idle = self.idle_workers.lock().await;
            let close_futures: Vec<_> = idle.drain(..)
                .map(|w| w.browser.close())
                .collect();

            let _ = futures::future::join_all(close_futures).await;
        }

        Ok(())
    }

    pub async fn wait_for_all(&self) -> Result<(), BrowserError> {
        // Wait until queue is empty and all workers are idle
        while self.queue_size().await > 0 || self.active_size().await > 0 {
            sleep(Duration::from_millis(100)).await;

            // Check if closed
            if *self.is_closed.lock().await {
                return Err(BrowserError::OperationError("Cluster is closed".to_string()));
            }
        }

        Ok(())
    }
}
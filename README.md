# Browser Cluster for Rust

A high-performance browser automation and clustering library for Rust, inspired by [puppeteer-cluster](https://github.com/thomasdondorf/puppeteer-cluster) for Node.js.

## Features

- **Efficient Browser Management**: Automatically manages browser instances with configurable concurrency limits
- **Smart Queueing System**: FIFO or LIFO queue modes with automatic retry capabilities
- **Resource Optimization**: Maintains a pool of idle browsers for instant task execution
- **Automatic Cleanup**: Handles browser lifecycle management with configurable timeouts
- **Multiple Browser Backends**: Support for popular browser automation libraries (Playwright, fantoccini, headless_chrome)
- **Fully Asynchronous**: Built with Tokio for maximum performance and scalability

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
browser-cluster = "0.1.0"
# Pick one of the following browser backends:
playwright = "0.0.20"
# or
fantoccini = "0.19.3" 
# or
headless_chrome = "1.0.6"
```

## Basic Usage

```rust
use browser_cluster::{Browser, BrowserError, Cluster, ClusterOptions, QueueMode};
use std::{sync::Arc, time::Duration};

// Implement the Browser trait for your chosen browser backend
#[derive(Clone)]
struct MyBrowser {
    // Your browser implementation
}

#[async_trait::async_trait]
impl Browser for MyBrowser {
    async fn new() -> Result<Self, BrowserError> {
        // Initialize your browser here
    }
    
    async fn close(&self) -> Result<(), BrowserError> {
        // Close your browser here
    }
}

// Define your task data
struct TaskData {
    url: String,
}

// Create a task handler function
async fn process_task(browser: Arc<MyBrowser>, data: TaskData) -> Result<(), BrowserError> {
    // Your browser automation logic here
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Configure the cluster
    let options = ClusterOptions {
        concurrency: 4,                                // Maximum concurrent tasks
        max_concurrent_browsers: 4,                    // Maximum browser instances
        min_idle_browsers: 1,                          // Keep at least one browser ready
        max_queue_size: Some(100),                     // Maximum queue size
        idle_timeout: Some(Duration::from_secs(60)),   // Idle timeout
        max_browser_lifetime: Some(Duration::from_secs(300)), // Max browser lifetime
        retry_limit: 2,                                // Retry failed tasks twice
        retry_delay: Duration::from_millis(500),       // Wait 500ms between retries
        queue_mode: QueueMode::FIFO,                   // First in, first out
        monitor_interval: Duration::from_secs(10),     // Check browser health every 10s
    };

    // Create the cluster
    let cluster = Cluster::<MyBrowser, TaskData>::new(options).await?;
    
    // Queue a task
    cluster.execute(process_task, TaskData { 
        url: "https://www.example.com".to_string() 
    }).await?;
    
    // Wait for all tasks to complete
    cluster.wait_for_all().await?;
    
    // Close the cluster
    cluster.close().await?;
    
    Ok(())
}
```

## Advanced Usage

### With Playwright

```rust
use browser_cluster::{Browser, BrowserError, Cluster, ClusterOptions};
use playwright::Playwright;
use std::sync::Arc;

#[derive(Clone)]
struct PlaywrightBrowser {
    browser: Arc<playwright::Browser>,
    playwright: Arc<Playwright>,
}

#[async_trait::async_trait]
impl Browser for PlaywrightBrowser {
    async fn new() -> Result<Self, BrowserError> {
        let playwright = Playwright::initialize().await
            .map_err(|e| BrowserError::LaunchError(e.to_string()))?;
        
        let browser = playwright.chromium()
            .launcher()
            .headless(true)
            .launch()
            .await
            .map_err(|e| BrowserError::LaunchError(e.to_string()))?;
        
        Ok(Self {
            browser: Arc::new(browser),
            playwright: Arc::new(playwright),
        })
    }
    
    async fn close(&self) -> Result<(), BrowserError> {
        self.browser.close().await
            .map_err(|e| BrowserError::OperationError(e.to_string()))
    }
}
```

## Performance Optimization

- **Browser Pre-warming**: Set `min_idle_browsers` to keep browsers ready for instant task execution
- **Concurrency Control**: Tune `concurrency` and `max_concurrent_browsers` based on system resources
- **Browser Lifecycle**: Adjust `idle_timeout` and `max_browser_lifetime` to balance resource usage
- **Queue Management**: Choose `QueueMode::LIFO` for prioritizing newer tasks or `QueueMode::FIFO` for fair scheduling

## License

This project is licensed under the MIT License - see the [LICENSE](./LICENSE) file for details.
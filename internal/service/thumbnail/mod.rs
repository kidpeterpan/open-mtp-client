mod disk;
#[cfg(test)]
mod fixtures;
mod mapper;
#[allow(dead_code)] // used by the video path (Task 5)
mod mp4;
mod service;
#[allow(dead_code)] // used by the video path (Task 5)
mod sparse;
#[cfg(target_os = "macos")]
#[allow(dead_code)] // used by the video path (Task 5)
mod video;

pub use disk::DISK_CACHE_MOST;
pub use service::Service;

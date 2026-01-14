use serde::{Serialize, Deserialize};
use std::path::PathBuf;
use fslock::LockFile;
use anyhow::Result;

pub trait FilePath {
    fn get_file_path() -> PathBuf;
}

pub trait Load {
    fn load_from_file(path: &PathBuf) -> Self
    where
        Self: Serialize + Default + for<'de> Deserialize<'de>,
    {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(obj) = ron::from_str(&data) {
                return obj;
            }
        }
        return Self::default();
    }
}

pub struct FileLock<T> {
    pub file: T,
    _lock: LockFile,
}

impl<T> FileLock<T> {
    pub fn new() -> Result<Self>
    where
        T: Serialize + FilePath + Load + Default + for<'de> Deserialize<'de>,
    {
        let path = T::get_file_path();
        let mut tries = 100;

        while tries > 0 {
            tries -= 1;
            match LockFile::open(&path) {
                Ok(_lock) => {
                    let file = T::load_from_file(&path);
                    return Ok(FileLock {
                        file,
                        _lock,
                    });
                }
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
        anyhow::bail!("Failed to acquire lock for file {:?}", path);
    }
}
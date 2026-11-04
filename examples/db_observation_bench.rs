//! Run against separate control and candidate release builds on a disposable DB.
use shellboy::storage::{DbWorker, StorageError};
use std::time::Instant;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let worker = DbWorker::open(dir.path().join("bench.db")).await?;
    worker.call(|db| {
        db.execute_batch("CREATE TABLE bench(id INTEGER PRIMARY KEY,value INTEGER); WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<9999) INSERT INTO bench SELECT x,x FROM n;")?;
        Ok::<_, StorageError>(())
    }).await?;
    for scan in [false, true] {
        for pass in 0..18 {
            let request = Instant::now();
            let work_ns = worker
                .call(move |db| {
                    let start = Instant::now();
                    if scan {
                        let mut s = db.prepare_cached("SELECT value FROM bench WHERE id < 1000")?;
                        for _ in 0..100 {
                            let mut rows = s.query([])?;
                            while let Some(row) = rows.next()? {
                                std::hint::black_box(row.get::<_, i64>(0)?);
                            }
                        }
                    } else {
                        let mut s = db.prepare_cached("SELECT value FROM bench WHERE id = ?1")?;
                        for id in 0..50000_i64 {
                            std::hint::black_box(
                                s.query_row([id % 10000], |r| r.get::<_, i64>(0))?,
                            );
                        }
                    }
                    Ok::<_, StorageError>(start.elapsed().as_nanos())
                })
                .await?;
            println!(
                "scan={scan} pass={pass} work_ns={work_ns} request_ns={}",
                request.elapsed().as_nanos()
            );
        }
    }
    worker.shutdown().await?;
    Ok(())
}

//! CI-only generated subject instrumentation. Never use this binary for release.
use std::{future::Future,sync::{atomic::{AtomicBool,AtomicU64,AtomicUsize,Ordering},Mutex,OnceLock},time::Instant};
use serde_json::{json,Value};
const N:usize=5;
const NAMES:[&str;N]=["ui","output","maintenance","index","unknown"];
static ACTIVE:AtomicBool=AtomicBool::new(false);
static PHASE:AtomicUsize=AtomicUsize::new(0);
static START:OnceLock<Instant>=OnceLock::new();
static ADMIT:[AtomicU64;N]=[const {AtomicU64::new(0)};N];
static EXEC:[AtomicU64;N]=[const {AtomicU64::new(0)};N];
static WORK:[AtomicU64;N]=[const {AtomicU64::new(0)};N];
static DRAW:AtomicU64=AtomicU64::new(0);
static BINS:[AtomicU64;2048]=[const {AtomicU64::new(0)};2048];
static DRAW_US:[AtomicU64;4096]=[const {AtomicU64::new(0)};4096];
static OUTPUT:AtomicU64=AtomicU64::new(0);
static CHECKPOINTS:AtomicU64=AtomicU64::new(0);
static UI_READ_TASKS:AtomicU64=AtomicU64::new(0);
static RO_SITES:Mutex<std::collections::BTreeMap<&'static str,u64>>=Mutex::new(std::collections::BTreeMap::new());
static RO_ADMIT:AtomicU64=AtomicU64::new(0);
static RO_EXEC:AtomicU64=AtomicU64::new(0);
static RO_NS:AtomicU64=AtomicU64::new(0);
static STATE:Mutex<(Option<(f64,f64)>,Option<Value>)>=Mutex::new((None,None));
static UNKNOWN:Mutex<std::collections::BTreeMap<&'static str,u64>>=Mutex::new(std::collections::BTreeMap::new());
tokio::task_local! {static TASK_KIND:usize;}
pub(crate) async fn scope<F:Future>(future:F)->F::Output {TASK_KIND.scope(0,future).await}
pub(crate) fn category<F>()->usize {
 if let Ok(kind)=TASK_KIND.try_with(|kind|*kind){return kind;}
 let name=std::any::type_name::<F>();
 if name.contains("::frontend::"){0}
 else if name.contains("::driver::shell_output::collect_shell"){1}
 else if name.contains("collect_receipts")||name.contains("::gc::")||name.contains("::Control::start::"){2}
 else if name.contains("::index::"){3}else{4}
}
pub(crate) fn admitted(category:usize){if ACTIVE.load(Ordering::Relaxed){ADMIT[category].fetch_add(1,Ordering::Relaxed);}}
pub(crate) struct Work {category:usize,start:Instant,enabled:bool,ro:bool}
pub(crate) fn executing(category:usize,name:&'static str)->Work {
 let enabled=ACTIVE.load(Ordering::Relaxed);
 if enabled {EXEC[category].fetch_add(1,Ordering::Relaxed);if category==4 {let mut unknown=UNKNOWN.lock().unwrap();if unknown.contains_key(name)||unknown.len()<64{*unknown.entry(name).or_default()+=1;}}}
 Work{category,start:Instant::now(),enabled,ro:false}
}
pub(crate) fn ui_read_admitted(){if ACTIVE.load(Ordering::Relaxed){UI_READ_TASKS.fetch_add(1,Ordering::Relaxed);}}
pub(crate) fn ro_scope(flags:rusqlite::OpenFlags,site:&'static str)->Option<Work> {
 if !flags.contains(rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY){return None;}
 let enabled=ACTIVE.load(Ordering::Relaxed);
 if enabled {RO_ADMIT.fetch_add(1,Ordering::Relaxed);RO_EXEC.fetch_add(1,Ordering::Relaxed);*RO_SITES.lock().unwrap().entry(site).or_default()+=1;}
 Some(Work{category:0,start:Instant::now(),enabled,ro:true})
}
impl Drop for Work {fn drop(&mut self){if self.enabled&&ACTIVE.load(Ordering::Relaxed){let ns=self.start.elapsed().as_nanos().min(u64::MAX as u128)as u64;if self.ro{RO_NS.fetch_add(ns,Ordering::Relaxed);}else{WORK[self.category].fetch_add(ns,Ordering::Relaxed);}}}}
pub(crate) fn draw(){if ACTIVE.load(Ordering::Relaxed){let index=DRAW.fetch_add(1,Ordering::Relaxed)as usize;let elapsed=START.get().unwrap().elapsed();if let Some(bin)=BINS.get(elapsed.as_secs()as usize){bin.fetch_add(1,Ordering::Relaxed);}if let Some(at)=DRAW_US.get(index){at.store(elapsed.as_micros().min(u64::MAX as u128)as u64,Ordering::Relaxed);}}}
pub(crate) fn checkpoint(){if ACTIVE.load(Ordering::Relaxed){CHECKPOINTS.fetch_add(1,Ordering::Relaxed);}}
pub(crate) fn output_commit(){if ACTIVE.load(Ordering::Relaxed){OUTPUT.fetch_add(1,Ordering::Relaxed);}}
fn cpu()->(f64,f64){unsafe{let mut usage:libc::rusage=std::mem::zeroed();assert_eq!(libc::getrusage(libc::RUSAGE_SELF,&mut usage),0);(usage.ru_utime.tv_sec as f64+usage.ru_utime.tv_usec as f64/1e6,usage.ru_stime.tv_sec as f64+usage.ru_stime.tv_usec as f64/1e6)}}
fn ack(byte:u8){if let Ok(fd)=std::env::var("SHELLBOY_CI_TUI_ACK_FD"){let fd:i32=fd.parse().unwrap();unsafe{assert_eq!(libc::write(fd,(&byte as *const u8).cast(),1),1);}}}
pub(crate) fn key(event:&crossterm::event::Event)->bool {
 if std::env::var_os("SHELLBOY_CI_TUI_METRICS").is_none(){return false;}
 if !matches!(event,crossterm::event::Event::Key(key)if key.code==crossterm::event::KeyCode::F(12)&&key.kind==crossterm::event::KeyEventKind::Press){return false;}
 match PHASE.load(Ordering::SeqCst){
 0=>{START.set(Instant::now()).unwrap();STATE.lock().unwrap().0=Some(cpu());PHASE.store(1,Ordering::SeqCst);ACTIVE.store(true,Ordering::SeqCst);ack(b'S');},
 1=>{ACTIVE.store(false,Ordering::SeqCst);let end=cpu();let wall=START.get().unwrap().elapsed().as_secs_f64();let mut state=STATE.lock().unwrap();let begin=state.0.unwrap();let draws=DRAW.load(Ordering::SeqCst);let categories=(0..N).map(|index|(NAMES[index].to_owned(),json!({"admitted":ADMIT[index].load(Ordering::SeqCst),"executed":EXEC[index].load(Ordering::SeqCst),"completed_work_ns":WORK[index].load(Ordering::SeqCst)}))).collect::<serde_json::Map<_,_>>();state.1=Some(json!({"version":"tui-measure-v1","source_witness":"SOURCE_WITNESS","wall_seconds":wall,"cpu_user_seconds":end.0-begin.0,"cpu_system_seconds":end.1-begin.1,"cpu_percent_one_core":100.0*(end.0+end.1-begin.0-begin.1)/wall,"draws":draws,"draw_bins":BINS.iter().take((wall.ceil()as usize).min(BINS.len())).map(|value|value.load(Ordering::Relaxed)).collect::<Vec<_>>(),"draw_first_us":DRAW_US.iter().take((draws as usize).min(DRAW_US.len())).map(|value|value.load(Ordering::Relaxed)).collect::<Vec<_>>(),"draw_timestamp_capacity":DRAW_US.len(),"db_categories":categories,"ui_read_tasks_admitted":UI_READ_TASKS.load(Ordering::SeqCst),"read_only_connections":{"admitted":RO_ADMIT.load(Ordering::SeqCst),"executed":RO_EXEC.load(Ordering::SeqCst),"completed_work_ns":RO_NS.load(Ordering::SeqCst)},"read_only_sites":*RO_SITES.lock().unwrap(),"harness_witness":"HARNESS_WITNESS","output_commits":OUTPUT.load(Ordering::SeqCst),"internal_maintenance_checkpoints":CHECKPOINTS.load(Ordering::SeqCst),"unknown_callers":*UNKNOWN.lock().unwrap(),"timing_note":"CPU is RUSAGE_SELF(all Shellboy threads, not tool/fixture children); all production open_with_flags sites are covered; readonly admitted/executed count actual open boundaries, UI read-task admission is counted separately; readonly work duration is lexical scope lifetime, not pure SQL time; one start ACK pipe write is included; no per-draw IO; work completing after end is not counted in completed_work_ns"}));PHASE.store(2,Ordering::SeqCst);ack(b'E');},_=>{}
 };true
}
pub(crate) struct FlushOnDrop;
impl Drop for FlushOnDrop {fn drop(&mut self){if let Some(path)=std::env::var_os("SHELLBOY_CI_TUI_METRICS"){if let Some(value)=STATE.lock().unwrap().1.as_ref(){std::fs::write(path,serde_json::to_vec_pretty(value).unwrap()).unwrap();}}}}

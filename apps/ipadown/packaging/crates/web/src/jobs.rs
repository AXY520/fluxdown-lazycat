use ipadown_store as store;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;

const MAX_CONCURRENT_DOWNLOADS: usize = 2;
const MAX_PENDING_DOWNLOADS: usize = 8;

#[derive(Clone, Serialize)]
pub(super) struct Job {
    id: String,
    bundle_id: String,
    app_name: Option<String>,
    version_id: Option<String>,
    platform: store::search::DevicePlatform,
    account: String,
    /// queued | downloading | done | error
    status: String,
    downloaded: u64,
    /// 预估安装包总字节数（来自历史版本目录或商店 lookup），供前端显示百分比
    #[serde(skip_serializing_if = "Option::is_none")]
    total: Option<u64>,
    error: Option<String>,
    /// 前端据此弹出双重认证对话框，而不是解析错误文案。
    needs_2fa: bool,
    #[serde(skip_serializing)]
    created_at: u64,
}

impl Job {
    fn new(
        id: &str,
        bundle_id: &str,
        version_id: Option<&str>,
        platform: store::search::DevicePlatform,
        account: &str,
        total: Option<u64>,
    ) -> Self {
        Job {
            id: id.to_string(),
            bundle_id: bundle_id.to_string(),
            app_name: None,
            version_id: version_id.map(String::from),
            platform,
            account: account.to_string(),
            status: "queued".into(),
            downloaded: 0,
            total,
            error: None,
            needs_2fa: false,
            created_at: unix_now(),
        }
    }

    fn record_failure(&mut self, error: &store::StoreError) {
        self.status = "error".into();
        self.needs_2fa = needs_two_factor(error);
        self.error = Some(if self.needs_2fa {
            "需要双重验证码，请完成验证后重试下载".into()
        } else {
            crate::friendly_msg(&error.to_string())
        });
    }
}

pub(super) fn needs_two_factor(error: &store::StoreError) -> bool {
    matches!(error, store::StoreError::CodeRequired)
}

#[derive(Clone)]
pub(super) struct Registry {
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    /// 每个活跃任务句柄，用于暂停/删除时中止子进程
    handles: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>,
    permits: Arc<Semaphore>,
}

pub(super) fn new_registry() -> Registry {
    Registry {
        jobs: Arc::new(Mutex::new(HashMap::new())),
        handles: Arc::new(Mutex::new(HashMap::new())),
        permits: Arc::new(Semaphore::new(MAX_CONCURRENT_DOWNLOADS)),
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

/// 锁中毒时数据仍完整，取回继续而不是连环 panic。
fn jobs_map(registry: &Registry) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
    registry
        .jobs
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn handles_map(
    registry: &Registry,
) -> std::sync::MutexGuard<'_, HashMap<String, tokio::task::AbortHandle>> {
    registry
        .handles
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(super) fn download_filename(
    bundle_id: &str,
    version_id: Option<&str>,
    platform: store::search::DevicePlatform,
) -> Result<String, &'static str> {
    if bundle_id.is_empty()
        || bundle_id.len() > 255
        || bundle_id.contains("..")
        || !bundle_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("非法 bundle id");
    }
    if let Some(version) = version_id {
        if version.is_empty()
            || version.len() > 32
            || !version.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err("非法版本 ID");
        }
    }
    Ok(format!(
        "{}_{}{}.ipa",
        bundle_id,
        version_id.unwrap_or("latest"),
        platform.file_suffix(),
    ))
}

pub(super) fn list(reg: &Registry) -> Vec<Job> {
    let reg = jobs_map(reg);
    let mut jobs: Vec<Job> = reg.values().cloned().collect();
    jobs.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    jobs
}

/// 账号重新认证成功后，移除该账号此前因 2FA 失败的任务。
pub(super) fn clear_two_factor_failures(reg: &Registry, account: &str) -> usize {
    let mut jobs = jobs_map(reg);
    let before = jobs.len();
    jobs.retain(|_, job| {
        !(job.needs_2fa && job.status == "error" && job.account.eq_ignore_ascii_case(account))
    });
    before - jobs.len()
}

pub(super) fn is_file_active(reg: &Registry, filename: &str) -> bool {
    jobs_map(reg).values().any(|job| {
        matches!(job.status.as_str(), "queued" | "downloading")
            && download_filename(&job.bundle_id, job.version_id.as_deref(), job.platform).as_deref()
                == Ok(filename)
    })
}

fn prune_finished(reg: &Registry) {
    const MAX_FINISHED_JOBS: usize = 100;
    let mut reg = jobs_map(reg);
    let mut finished: Vec<(u64, String)> = reg
        .values()
        .filter(|job| matches!(job.status.as_str(), "done" | "error"))
        .map(|job| (job.created_at, job.id.clone()))
        .collect();
    if finished.len() <= MAX_FINISHED_JOBS {
        return;
    }
    finished.sort();
    let remove_count = finished.len() - MAX_FINISHED_JOBS;
    for (_, id) in finished.into_iter().take(remove_count) {
        reg.remove(&id);
    }
}

fn update(reg: &Registry, id: &str, f: impl FnOnce(&mut Job)) {
    if let Some(job) = jobs_map(reg).get_mut(id) {
        f(job);
    }
}

/// 在同一临界区内完成去重、容量检查和预留。
/// `Ok(None)` 表示相同任务已在队列中，`Ok(Some(id))` 表示创建了新任务。
fn reserve_job(
    reg: &Registry,
    bundle_id: &str,
    version_id: Option<&str>,
    platform: store::search::DevicePlatform,
    account_email: &str,
    total: Option<u64>,
) -> Result<Option<String>, &'static str> {
    let mut jobs = jobs_map(reg);
    if jobs.values().any(|job| {
        job.bundle_id == bundle_id
            && job.version_id.as_deref() == version_id
            && job.platform == platform
            && matches!(job.status.as_str(), "queued" | "downloading")
    }) {
        return Ok(None);
    }
    let pending = jobs
        .values()
        .filter(|job| matches!(job.status.as_str(), "queued" | "downloading"))
        .count();
    if pending >= MAX_PENDING_DOWNLOADS {
        return Err("下载队列已满，请等待现有任务完成后重试");
    }
    let id = crate::random_id();
    jobs.insert(
        id.clone(),
        Job::new(&id, bundle_id, version_id, platform, account_email, total),
    );
    Ok(Some(id))
}

/// 启动后台下载任务（走 ipatool 子进程，含 SAP 签名），返回是否复用已有任务。
/// `total` 为预估安装包大小（字节），供前端进度条显示百分比。
pub(super) fn spawn_download(
    reg: Registry,
    bundle_id: String,
    version_id: Option<String>,
    platform: store::search::DevicePlatform,
    account_email: String,
    total: Option<u64>,
    ipatool: store::ipatool::Ipatool,
) -> Result<bool, &'static str> {
    let filename = download_filename(&bundle_id, version_id.as_deref(), platform)?;
    let Some(id) = reserve_job(
        &reg,
        &bundle_id,
        version_id.as_deref(),
        platform,
        &account_email,
        total,
    )?
    else {
        return Ok(true);
    };

    launch_download_task(&reg, id, bundle_id, version_id, platform, filename, ipatool);
    Ok(false)
}

/// 启动实际执行下载的 tokio 任务并登记句柄。
/// 任务结束时自动注销句柄；暂停/删除时用句柄中止（子进程随 kill_on_drop 终止）。
#[allow(clippy::too_many_arguments)]
fn launch_download_task(
    reg: &Registry,
    id: String,
    bundle_id: String,
    version_id: Option<String>,
    platform: store::search::DevicePlatform,
    filename: String,
    ipatool: store::ipatool::Ipatool,
) {
    let task_reg = reg.clone();
    let task_id = id.clone();
    let handle = tokio::spawn(async move {
        match task_reg.permits.clone().acquire_owned().await {
            Ok(_permit) => {
                if let Err(e) = run_download(
                    &task_reg,
                    &task_id,
                    &bundle_id,
                    version_id.as_deref(),
                    platform,
                    filename,
                    ipatool,
                )
                .await
                {
                    store::log_error!(
                        "download",
                        "job failed id={task_id} bundle={bundle_id} version={} platform={} error={e}",
                        version_id.as_deref().unwrap_or("latest"),
                        platform.ipatool_value()
                    );
                    update(&task_reg, &task_id, |job| job.record_failure(&e));
                }
            }
            Err(_) => update(&task_reg, &task_id, |job| {
                job.record_failure(&store::StoreError::UnexpectedResponse(
                    "下载队列已停止".into(),
                ));
            }),
        }
        prune_finished(&task_reg);
        handles_map(&task_reg).remove(&task_id);
    });
    handles_map(reg).insert(id, handle.abort_handle());
}

/// 暂停：中止任务与 ipatool 子进程，保留 .tmp 断点，状态置 paused。
pub(super) fn pause_job(reg: &Registry, id: &str) -> Result<(), &'static str> {
    let mut jobs = jobs_map(reg);
    let Some(job) = jobs.get_mut(id) else {
        return Err("任务不存在");
    };
    if !matches!(job.status.as_str(), "queued" | "downloading") {
        return Err("任务不在进行中，无法暂停");
    }
    if let Some(handle) = handles_map(reg).remove(id) {
        handle.abort();
    }
    job.status = "paused".into();
    job.error = None;
    store::log_info!("download", "job paused id={id}");
    Ok(())
}

/// 继续已暂停的任务：复用原任务记录，ipatool 从 .tmp 断点续传。
pub(super) fn resume_job(
    reg: &Registry,
    id: &str,
    ipatool: store::ipatool::Ipatool,
) -> Result<(), &'static str> {
    {
        let jobs = jobs_map(reg);
        let Some(job) = jobs.get(id) else {
            return Err("任务不存在");
        };
        if job.status != "paused" {
            return Err("任务未处于暂停状态");
        }
        let duplicated = jobs.values().any(|other| {
            other.id != id
                && other.bundle_id == job.bundle_id
                && other.version_id.as_deref() == job.version_id.as_deref()
                && other.platform == job.platform
                && matches!(other.status.as_str(), "queued" | "downloading")
        });
        if duplicated {
            return Err("相同任务已在进行中");
        }
    }
    let (bundle_id, version_id, platform) = {
        let mut jobs = jobs_map(reg);
        let job = jobs.get_mut(id).expect("任务刚校验过");
        job.status = "queued".into();
        job.error = None;
        (job.bundle_id.clone(), job.version_id.clone(), job.platform)
    };
    let filename = download_filename(&bundle_id, version_id.as_deref(), platform)?;
    store::log_info!("download", "job resumed id={id}");
    launch_download_task(
        reg,
        id.to_string(),
        bundle_id,
        version_id,
        platform,
        filename,
        ipatool,
    );
    Ok(())
}

/// 删除任务记录；进行中的任务会先被中止。
pub(super) fn remove_job(reg: &Registry, id: &str) -> bool {
    let removed = jobs_map(reg).remove(id).is_some();
    if removed {
        if let Some(handle) = handles_map(reg).remove(id) {
            handle.abort();
        }
        store::log_info!("download", "job removed id={id}");
    }
    removed
}

/// 任务归属账号（用于继续下载时构建对应账号的 ipatool）。
pub(super) fn job_account(reg: &Registry, id: &str) -> Option<String> {
    jobs_map(reg).get(id).map(|job| job.account.clone())
}

async fn run_download(
    reg: &Registry,
    id: &str,
    bundle_id: &str,
    version_id: Option<&str>,
    platform: store::search::DevicePlatform,
    filename: String,
    ipatool: store::ipatool::Ipatool,
) -> store::Result<()> {
    let started = std::time::Instant::now();
    store::log_info!(
        "download",
        "job started id={id} bundle={bundle_id} version={} platform={} file={filename}",
        version_id.unwrap_or("latest"),
        platform.ipatool_value()
    );
    // 入队前同步校验会话（auth info + 真实商店请求），失败/2FA 直接让任务失败，
    // 前端轮询 needs_2fa 后弹验证码对话框。
    ipatool.validate_session().await?;
    // 用公开搜索 API 补充应用名（多区依次尝试）；未提供大小时顺带拿到当前版本包大小
    for country in ["CN", "US", "JP"] {
        if let Ok(app) = store::search::lookup(bundle_id, country, platform).await {
            update(reg, id, |j| {
                j.app_name = Some(app.name);
                if j.total.is_none() {
                    j.total = app.file_size_bytes;
                }
            });
            break;
        }
    }

    update(reg, id, |j| j.status = "downloading".into());
    let output = store::config::ipas_dir()?.join(&filename);

    // 进度监视：ipatool 先下载到 <out>.tmp（支持断点续传），完成后才组装为最终 IPA
    let tmp_path = output.with_file_name(format!("{filename}.tmp"));
    let watch_output = output.clone();
    let reg_w = reg.clone();
    let id_w = id.to_string();
    let watcher = tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            // 任务被暂停/删除后自动退出，避免残留监视协程
            let active = jobs_map(&reg_w)
                .get(&id_w)
                .is_some_and(|job| matches!(job.status.as_str(), "queued" | "downloading"));
            if !active {
                break;
            }
            let size = match tokio::fs::metadata(&tmp_path).await {
                Ok(meta) => meta.len(),
                Err(_) => tokio::fs::metadata(&watch_output)
                    .await
                    .map(|meta| meta.len())
                    .unwrap_or(0),
            };
            if size > 0 {
                update(&reg_w, &id_w, |j| j.downloaded = size);
            }
        }
    });
    let result = ipatool
        .download(bundle_id, version_id, &output, platform)
        .await;
    watcher.abort();
    result?;

    let size = std::fs::metadata(&output).map(|m| m.len()).unwrap_or(0);
    update(reg, id, |j| {
        j.status = "done".into();
        j.downloaded = size;
    });
    store::log_info!(
        "download",
        "job done id={id} bundle={bundle_id} file={filename} size={size} elapsed={}s",
        started.elapsed().as_secs()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        clear_two_factor_failures, download_filename, job_account, jobs_map, list, new_registry,
        pause_job, remove_job, reserve_job, Job, MAX_PENDING_DOWNLOADS,
    };
    use ipadown_store::search::DevicePlatform;
    use ipadown_store::StoreError;

    #[test]
    fn download_filename_accepts_store_identifiers() {
        assert_eq!(
            download_filename("com.example.app", Some("123456789"), DevicePlatform::Iphone)
                .unwrap(),
            "com.example.app_123456789.ipa"
        );
        assert_eq!(
            download_filename("com.example.app", None, DevicePlatform::Ipad).unwrap(),
            "com.example.app_latest_ipad.ipa"
        );
        assert_eq!(
            download_filename("com.example.app", Some("123"), DevicePlatform::Appletv).unwrap(),
            "com.example.app_123_appletv.ipa"
        );
    }

    #[test]
    fn download_filename_rejects_path_traversal() {
        assert!(download_filename("../outside", None, DevicePlatform::Iphone).is_err());
        assert!(download_filename(
            "com.example.app",
            Some("../../outside"),
            DevicePlatform::Iphone
        )
        .is_err());
        assert!(download_filename("com/example/app", Some("123"), DevicePlatform::Iphone).is_err());
    }

    #[test]
    fn two_factor_failure_is_structured_for_frontend() {
        let mut job = Job::new(
            "job-1",
            "com.example.app",
            Some("123"),
            DevicePlatform::Iphone,
            "a@example.com",
            None,
        );

        job.record_failure(&StoreError::CodeRequired);

        assert_eq!(job.status, "error");
        assert!(job.needs_2fa);
        assert!(job.error.is_some());
    }

    #[test]
    fn serialized_job_hides_internal_sorting_state() {
        let job = Job::new(
            "job-1",
            "com.example.app",
            Some("123"),
            DevicePlatform::Iphone,
            "a@example.com",
            None,
        );
        let value = serde_json::to_value(job).unwrap();

        assert!(value.get("created_at").is_none());
        assert_eq!(value["status"], "queued");
    }

    #[test]
    fn successful_authentication_clears_matching_two_factor_failures() {
        let registry = new_registry();
        let mut job = Job::new(
            "job-1",
            "com.example.app",
            Some("123"),
            DevicePlatform::Iphone,
            "a@example.com",
            None,
        );
        job.record_failure(&StoreError::CodeRequired);
        let mut other_error = Job::new(
            "job-2",
            "com.example.other",
            None,
            DevicePlatform::Iphone,
            "a@example.com",
            None,
        );
        other_error.record_failure(&StoreError::AuthFailed("network".into()));
        let mut other_account = Job::new(
            "job-3",
            "com.example.third",
            None,
            DevicePlatform::Iphone,
            "b@example.com",
            None,
        );
        other_account.record_failure(&StoreError::CodeRequired);
        let mut jobs = registry.jobs.lock().unwrap();
        jobs.insert(job.id.clone(), job);
        jobs.insert(other_error.id.clone(), other_error);
        jobs.insert(other_account.id.clone(), other_account);
        drop(jobs);

        assert_eq!(clear_two_factor_failures(&registry, "a@example.com"), 1);
        let remaining = list(&registry);
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|job| job.id != "job-1"));
    }

    #[test]
    fn queue_deduplicates_and_enforces_pending_limit() {
        let registry = new_registry();
        assert!(reserve_job(
            &registry,
            "com.example.app",
            Some("1"),
            DevicePlatform::Iphone,
            "a@example.com",
            None
        )
        .unwrap()
        .is_some());
        assert!(reserve_job(
            &registry,
            "com.example.app",
            Some("1"),
            DevicePlatform::Iphone,
            "a@example.com",
            None
        )
        .unwrap()
        .is_none());

        for index in 1..MAX_PENDING_DOWNLOADS {
            assert!(reserve_job(
                &registry,
                &format!("com.example.app{index}"),
                None,
                DevicePlatform::Iphone,
                "a@example.com",
                None
            )
            .unwrap()
            .is_some());
        }
        assert!(reserve_job(
            &registry,
            "com.example.overflow",
            None,
            DevicePlatform::Iphone,
            "a@example.com",
            None
        )
        .is_err());
    }

    #[test]
    fn pause_and_remove_manage_job_lifecycle() {
        let registry = new_registry();
        let mut job = Job::new(
            "job-1",
            "com.example.app",
            Some("1"),
            DevicePlatform::Iphone,
            "a@example.com",
            None,
        );
        job.status = "downloading".into();
        jobs_map(&registry).insert(job.id.clone(), job);

        assert!(pause_job(&registry, "job-1").is_ok());
        assert_eq!(list(&registry)[0].status, "paused");
        assert!(pause_job(&registry, "job-1").is_err());
        assert_eq!(
            job_account(&registry, "job-1").as_deref(),
            Some("a@example.com")
        );
        assert!(remove_job(&registry, "job-1"));
        assert!(list(&registry).is_empty());
        assert!(!remove_job(&registry, "job-1"));
    }

    #[test]
    fn queue_keeps_same_version_for_different_platforms_separate() {
        let registry = new_registry();
        assert!(reserve_job(
            &registry,
            "com.example.app",
            Some("1"),
            DevicePlatform::Iphone,
            "a@example.com",
            None
        )
        .unwrap()
        .is_some());
        assert!(reserve_job(
            &registry,
            "com.example.app",
            Some("1"),
            DevicePlatform::Ipad,
            "a@example.com",
            None
        )
        .unwrap()
        .is_some());
    }
}

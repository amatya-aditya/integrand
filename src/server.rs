//! gRPC client for `latex_ocr_server` and the lifecycle of its Python process.
//! Messages are declared by hand so the build doesn't need protoc; they mirror protos/latex_ocr.proto.

use crate::settings::ServerSettings as ServerConfig;
use anyhow::{Result, anyhow, bail};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tonic::codec::ProstCodec;
use tonic::codegen::http::uri::PathAndQuery;
use tonic::transport::{Channel, Endpoint};

#[derive(Clone, PartialEq, prost::Message)]
struct LatexRequest {
    #[prost(string, tag = "1")]
    image_path: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct LatexReply {
    #[prost(string, tag = "1")]
    latex: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct ServerIsReadyReply {
    #[prost(bool, tag = "1")]
    is_ready: bool,
}
#[derive(Clone, PartialEq, prost::Message)]
struct ServerConfigReply {
    #[prost(string, tag = "1")]
    device: String,
    #[prost(string, tag = "2")]
    cache_dir: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct Empty {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Health {
    Ready,
    /// Listening but the model is still loading.
    Loading,
    Unreachable,
}

pub struct Server {
    cfg: ServerConfig,
    channel: Channel,
    child: Mutex<Option<Child>>,
}

impl Server {
    pub fn new(cfg: ServerConfig) -> Self {
        // Lazy channel: reconnects by itself, so it survives server restarts (ours or another app's).
        let channel = Endpoint::from_shared(format!("http://127.0.0.1:{}", cfg.port))
            .expect("valid uri")
            .connect_timeout(Duration::from_millis(300))
            .tcp_nodelay(true)
            .connect_lazy();
        Self {
            cfg,
            channel,
            child: Mutex::new(None),
        }
    }

    async fn unary<Req, Resp>(&self, path: &'static str, req: Req, timeout: Duration) -> Result<Resp>
    where
        Req: prost::Message + Send + Sync + 'static,
        Resp: prost::Message + Default + Send + Sync + 'static,
    {
        let mut grpc = tonic::client::Grpc::new(self.channel.clone());
        tokio::time::timeout(timeout, grpc.ready())
            .await
            .map_err(|_| anyhow!("server not reachable"))?
            .map_err(|e| anyhow!("server not reachable: {e}"))?;
        let mut request = tonic::Request::new(req);
        request.set_timeout(timeout);
        let codec: ProstCodec<Req, Resp> = ProstCodec::default();
        let reply = grpc.unary(request, PathAndQuery::from_static(path), codec).await?;
        Ok(reply.into_inner())
    }

    pub async fn health(&self) -> Health {
        match self
            .unary::<_, ServerIsReadyReply>("/latexocr.LatexOCR/IsReady", Empty {}, Duration::from_millis(500))
            .await
        {
            Ok(r) if r.is_ready => Health::Ready,
            Ok(_) => Health::Loading,
            Err(_) => Health::Unreachable,
        }
    }

    /// "cuda" / "cpu" as reported by the server.
    pub async fn device(&self) -> Option<String> {
        let r: ServerConfigReply = self
            .unary("/latexocr.LatexOCR/GetConfig", Empty {}, Duration::from_millis(800))
            .await
            .ok()?;
        Some(r.device)
    }

    pub fn port(&self) -> u16 {
        self.cfg.port
    }

    pub async fn generate_latex(&self, image_path: &str) -> Result<String> {
        let req = LatexRequest {
            image_path: image_path.to_owned(),
        };
        let reply: LatexReply = self.unary("/latexocr.LatexOCR/GenerateLatex", req, Duration::from_secs(60)).await?;
        Ok(reply.latex.trim().to_owned())
    }

    /// True while a server process we spawned is still alive.
    fn own_child_alive(&self) -> bool {
        let mut child = self.child.lock().unwrap();
        match child.as_mut().map(|c| c.try_wait()) {
            Some(Ok(None)) => true,
            Some(Ok(Some(status))) => {
                log::warn!("server process exited: {status}");
                *child = None;
                false
            }
            _ => false,
        }
    }

    /// Start the Python server unless one is already running (ours, or another app's on the same port).
    pub async fn ensure_started(&self) -> Result<()> {
        if self.own_child_alive() || self.health().await != Health::Unreachable {
            return Ok(());
        }
        if !self.cfg.autostart {
            bail!("OCR server is not running on port {} (autostart is off)", self.cfg.port);
        }
        self.spawn()
    }

    fn spawn(&self) -> Result<()> {
        let mut parts = self.cfg.python.split_whitespace(); // allows e.g. "py -3.14"
        let exe = parts.next().ok_or_else(|| anyhow!("server.python is empty"))?;
        let mut cmd = Command::new(exe);
        cmd.args(parts)
            .args(["-m", "latex_ocr_server", "start", "-d", "--port", &self.cfg.port.to_string()]);
        if !self.cfg.cache_dir.is_empty() {
            cmd.args(["--cache_dir", &self.cfg.cache_dir]);
        }
        if self.cfg.cpu {
            cmd.arg("--cpu");
        }
        // Model already downloaded: skip the Hugging Face Hub round-trips at startup.
        let cache = match self.cfg.cache_dir.as_str() {
            "" => dirs::home_dir().unwrap_or_default().join(".cache/huggingface/hub"),
            dir => dir.into(),
        };
        if cache.join("models--Norm--nougat-latex-base").is_dir() {
            cmd.env("HF_HUB_OFFLINE", "1");
        }
        let log = std::fs::File::create(crate::settings::data_dir().join("server.log"))?;
        cmd.stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .env("PYTHONUNBUFFERED", "1");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        log::info!("starting server: {cmd:?}");
        let child = cmd.spawn().map_err(|e| anyhow!("could not start `{exe}`: {e}"))?;
        crate::platform::tie_to_app(&child);
        *self.child.lock().unwrap() = Some(child);
        Ok(())
    }

    /// Start the server if needed and wait until the model is loaded.
    pub async fn wait_ready(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.health().await {
                Health::Ready => return Ok(()),
                Health::Unreachable => self.ensure_started().await?,
                Health::Loading => {}
            }
            if Instant::now() > deadline {
                bail!("OCR server did not become ready (see server.log)");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            log::info!("stopping server");
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn owns_process(&self) -> bool {
        self.own_child_alive()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

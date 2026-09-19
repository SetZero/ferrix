//! A client of virglrenderer's own test server, for running a stream on the
//! host.
//!
//! `virgl_test_server` is virglrenderer with a Unix socket where QEMU's
//! virtio-gpu would be: it takes the same command streams a guest's
//! `VIRTGPU_EXECBUFFER` carries and runs them on the host's GL. So what this
//! crate writes -- and the shaders above all, which are text only
//! virglrenderer can judge -- is tested in a second against the very code
//! that will run it, where a guest boot takes a minute and shows a black
//! screen for every mistake.
//!
//! This speaks the server's protocol version 0, the one it starts in: the
//! client numbers its own resources, and pixels ride in the socket rather
//! than in shared memory, so nothing here needs a descriptor passed or a
//! line of `unsafe`. It is for tests. A host without the server has none of
//! them, and [`Vtest::start`] says so with `None` rather than failing.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Device, Region, Texture, pipe};

/// The server's name, looked for on `PATH`.
const SERVER: &str = "virgl_test_server";

/// `VCMD_*`, from `vtest_protocol.h`.
const RESOURCE_CREATE: u32 = 2;
const TRANSFER_GET: u32 = 4;
const TRANSFER_PUT: u32 = 5;
const SUBMIT_CMD: u32 = 6;
const CREATE_RENDERER: u32 = 8;

/// A running server and the connection to it. The server is this object's
/// own and goes when it does.
#[derive(Debug)]
pub struct Vtest {
    child: Child,
    stream: UnixStream,
    socket: PathBuf,
    next: u32,
}

impl Drop for Vtest {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Vtest {
    /// Start a server of this test's own and connect to it.
    ///
    /// `None` when the host has no `virgl_test_server`, or no GL for it to
    /// stand on: a test that cannot run is skipped and says so, because the
    /// encoder's own word-for-word tests do not need it.
    ///
    /// # Errors
    ///
    /// The server started and then could not be spoken to.
    pub fn start(name: &str) -> io::Result<Option<Self>> {
        let socket =
            std::env::temp_dir().join(format!("ferrix-vtest-{}-{name}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket);
        let spawned = Command::new(SERVER)
            .arg("--no-fork")
            .arg("--use-egl-surfaceless")
            .arg("--socket-path")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // The socket appears once the server is listening.
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            if let Ok(stream) = UnixStream::connect(&socket) {
                break stream;
            }
            // A server that has already gone had no GL to start on.
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut vtest = Self {
            child,
            stream,
            socket,
            next: 1,
        };
        // The one command whose length is in bytes: the client's name.
        vtest.header(u32::try_from(name.len()).unwrap_or(0), CREATE_RENDERER)?;
        vtest.stream.write_all(name.as_bytes())?;
        Ok(Some(vtest))
    }

    fn header(&mut self, len: u32, command: u32) -> io::Result<()> {
        self.words(&[len, command])
    }

    fn words(&mut self, words: &[u32]) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(words.len() * 4);
        for word in words {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        self.stream.write_all(&bytes)
    }

    /// Make a resource and answer its number, which is what a stream names
    /// it by. A texture is `width` by `height`; a buffer is `width` bytes
    /// and one high.
    ///
    /// # Errors
    ///
    /// The socket's.
    pub fn create(
        &mut self,
        target: u32,
        format: u32,
        bind: u32,
        width: u32,
        height: u32,
    ) -> io::Result<u32> {
        let resource = self.next;
        self.next += 1;
        self.header(10, RESOURCE_CREATE)?;
        // Depth and array size of one, no mip levels, no samples.
        self.words(&[resource, target, format, bind, width, height, 1, 1, 0, 0])?;
        Ok(resource)
    }

    /// Run a stream.
    ///
    /// # Errors
    ///
    /// The socket's. What the *renderer* made of the stream it says on its
    /// standard error and nowhere else, which is why a test reads pixels.
    pub fn submit(&mut self, words: &[u32]) -> io::Result<()> {
        self.header(u32::try_from(words.len()).unwrap_or(0), SUBMIT_CMD)?;
        self.words(words)
    }

    /// Write `data`, rows `stride` bytes apart, into `region` of a texture.
    ///
    /// # Errors
    ///
    /// The socket's.
    pub fn put(
        &mut self,
        resource: u32,
        region: Region,
        stride: u32,
        data: &[u8],
    ) -> io::Result<()> {
        let len = u32::try_from(data.len()).unwrap_or(0);
        self.header(11 + len.div_ceil(4), TRANSFER_PUT)?;
        self.words(&[
            resource,
            0,
            stride,
            0,
            region.x,
            region.y,
            0,
            region.width,
            region.height,
            1,
            len,
        ])?;
        self.stream.write_all(data)
    }

    /// Read `region` of a texture back, four bytes a pixel, rows packed.
    /// This comes after every stream submitted before it.
    ///
    /// # Errors
    ///
    /// The socket's, including a server that died of the stream.
    pub fn get(&mut self, resource: u32, region: Region) -> io::Result<Vec<u8>> {
        let len = region.width * region.height * 4;
        self.header(11, TRANSFER_GET)?;
        self.words(&[
            resource,
            0,
            region.width * 4,
            0,
            region.x,
            region.y,
            0,
            region.width,
            region.height,
            1,
            len,
        ])?;
        let mut data = vec![0_u8; len as usize];
        self.stream.read_exact(&mut data)?;
        Ok(data)
    }
}

impl Device for Vtest {
    fn texture(&mut self, texture: Texture) -> io::Result<u32> {
        self.create(
            pipe::TEXTURE_2D,
            texture.format,
            texture.bind,
            texture.width,
            texture.height,
        )
    }

    fn buffer(&mut self, bytes: u32) -> io::Result<u32> {
        self.create(
            pipe::BUFFER,
            pipe::FORMAT_R8_UNORM,
            pipe::BIND_VERTEX_BUFFER,
            bytes,
            1,
        )
    }

    fn upload(
        &mut self,
        resource: u32,
        region: Region,
        stride: u32,
        data: &[u8],
    ) -> io::Result<()> {
        // The socket carries exactly the bytes the region covers: whole
        // strides for every row but the last, and that one's pixels.
        let rows = region.height.saturating_sub(1) as usize;
        let needed = rows * stride as usize + region.width as usize * 4;
        let data = data
            .get(..needed)
            .ok_or_else(|| io::Error::other("pixels too short for their region"))?;
        self.put(resource, region, stride, data)
    }

    fn submit(&mut self, words: &[u32]) -> io::Result<()> {
        Self::submit(self, words)
    }

    fn read(&mut self, resource: u32, region: Region) -> io::Result<Vec<u8>> {
        self.get(resource, region)
    }
}

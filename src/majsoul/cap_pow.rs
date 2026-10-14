//! GPU acceleration of the widget's SHA-256 nonce search, not instrumentation.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

pub(super) struct Solver {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::ComputePipeline,
    input: wgpu::Buffer,
    answer: wgpu::Buffer,
    readback: wgpu::Buffer,
    bindings: wgpu::BindGroup,
}

const BATCH: u32 = 16_384;
impl Solver {
    pub(super) async fn new() -> Result<Self> {
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        descriptor.backends = wgpu::Backends::VULKAN;
        let instance = wgpu::Instance::new(descriptor);
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions::default())
            .await?;
        anyhow::ensure!(
            adapter.get_info().device_type != wgpu::DeviceType::Cpu,
            "GPU adapter unavailable"
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor::default())
            .await?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Cap SHA256"),
            source: wgpu::ShaderSource::Wgsl(include_str!("cap_pow.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Cap nonce search"),
            layout: None,
            module: &shader,
            entry_point: Some("search"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let input = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 560,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let answer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 4,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bindings = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: answer.as_entire_binding(),
                },
            ],
        });
        Ok(Self {
            device,
            queue,
            pipeline,
            input,
            answer,
            readback,
            bindings,
        })
    }

    pub(super) fn solve(&self, salt: &str, target: &str) -> Result<u32> {
        anyhow::ensure!(
            salt.len() <= 128 && !target.is_empty() && target.len() <= 64,
            "unsupported SHA256 challenge size"
        );
        let mut words = [0u32; 140];
        for (i, byte) in salt.bytes().enumerate() {
            words[i] = u32::from(byte);
        }
        for (i, digit) in target.bytes().enumerate() {
            let nibble = char::from(digit)
                .to_digit(16)
                .context("invalid SHA256 target")?;
            words[128 + i / 8] |= nibble << (28 - (i % 8) * 4);
        }
        words[136] = u32::try_from(salt.len())?;
        words[137] = u32::try_from(target.len())?;
        // Small dispatches bound interference with the user's active miner.
        let started = Instant::now();
        for base in (0..u32::MAX - BATCH).step_by(BATCH as usize) {
            anyhow::ensure!(
                started.elapsed() < Duration::from_secs(20),
                "GPU nonce search timed out"
            );
            words[138] = base;
            words[139] = BATCH;
            let mut packed = [0u8; 560];
            for (word, bytes) in words.iter().zip(packed.as_chunks_mut::<4>().0) {
                bytes.copy_from_slice(&word.to_le_bytes());
            }
            self.queue.write_buffer(&self.input, 0, &packed);
            self.queue
                .write_buffer(&self.answer, 0, &u32::MAX.to_le_bytes());
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bindings, &[]);
                pass.dispatch_workgroups(BATCH / 64, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&self.answer, 0, &self.readback, 0, 4);
            let submission = self.queue.submit([encoder.finish()]);
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            self.readback
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    let _ = tx.send(result);
                });
            self.device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(5)),
            })?;
            rx.recv_timeout(Duration::from_secs(5))??;
            let data = self.readback.slice(..).get_mapped_range()?;
            let nonce = u32::from_le_bytes(data[..4].try_into()?);
            drop(data);
            self.readback.unmap();
            if nonce != u32::MAX {
                let digest = hex::encode(Sha256::digest(format!("{salt}{nonce}").as_bytes()));
                anyhow::ensure!(
                    digest.starts_with(&target.to_ascii_lowercase()),
                    "GPU SHA256 verification failed"
                );
                return Ok(nonce);
            }
        }
        anyhow::bail!("GPU nonce range exhausted")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Explicit opt-in keeps ordinary test runs independent of GPU hardware.
    #[tokio::test]
    #[ignore = "requires a Vulkan GPU"]
    async fn gpu_sha256_prefix_boundaries() -> Result<()> {
        let solver = Solver::new().await?;
        for length in [0, 16, 54, 55, 63, 64, 119, 128] {
            let salt = "a".repeat(length);
            let target = &hex::encode(Sha256::digest(format!("{salt}999").as_bytes()))[..3];
            let cpu_started = Instant::now();
            let expected = (0u32..)
                .find(|nonce| {
                    hex::encode(Sha256::digest(format!("{salt}{nonce}").as_bytes()))
                        .starts_with(target)
                })
                .context("CPU search exhausted")?;
            let cpu_us = cpu_started.elapsed().as_micros();
            let gpu_started = Instant::now();
            let actual = solver.solve(&salt, target)?;
            assert_eq!(actual, expected, "salt length {length}");
            eprintln!(
                "salt_length={length} cpu_us={cpu_us} gpu_us={}",
                gpu_started.elapsed().as_micros()
            );
        }
        Ok(())
    }
}

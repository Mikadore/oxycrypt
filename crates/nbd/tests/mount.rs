use std::os::unix::fs::FileExt;
use std::time::Duration;

use device_mem::MemoryDeviceBuilder;
use nbd::NbdError;
use nbd::Result;
use nbd::server::NbdServer;

#[test]
#[ignore = "requires root privileges and a free /dev/nbd0 device"]
fn mounts_a_memory_device_end_to_end() -> Result<()> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(NbdError::from)?;

    runtime.block_on(async {
        let built_device = MemoryDeviceBuilder::new()
            .block_size(512)
            .block_count(8)
            .initial_data(vec![0x5A; 4096])
            .build()
            .map_err(NbdError::from)?;
        let server = NbdServer::mount(0, built_device)?;
        let mut controller = server.run();

        tokio::time::sleep(Duration::from_millis(100)).await;

        let block_device = std::fs::OpenOptions::new()
            .read(true)
            .open("/dev/nbd0")
            .map_err(NbdError::from)?;
        let mut bytes = [0u8; 16];
        block_device.read_at(&mut bytes, 0).map_err(NbdError::from)?;
        assert_eq!(bytes, [0x5A; 16]);

        controller.stop().map_err(|err| {
            NbdError::from(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                format!("failed to send stop command: {err}"),
            ))
        })?;

        (&mut controller.handle).await.map_err(NbdError::from)??;
        Ok(())
    })
}

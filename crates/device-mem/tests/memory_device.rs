use block_device::BlockDevice;
use block_device::Durability;
use device_mem::MemoryDeviceBuilder;

#[test]
fn builder_returns_initialized_device_and_geometry() {
    let built = MemoryDeviceBuilder::new()
        .block_size(512)
        .block_count(8)
        .initial_data(vec![0xAA; 4096])
        .build()
        .expect("builder should succeed");

    assert_eq!(built.block_size, 512);
    assert_eq!(built.block_count, 8);

    let data = built
        .device
        .read_at(0, 16)
        .expect("read should succeed after initialization");
    assert_eq!(data, vec![0xAA; 16]);
}

#[test]
fn memory_device_round_trips_reads_and_writes() {
    let built = MemoryDeviceBuilder::new()
        .block_size(512)
        .block_count(4)
        .build()
        .expect("builder should succeed");

    built
        .device
        .write_at(512, &[1, 2, 3, 4], Durability::Buffered)
        .expect("write should succeed");
    built.device.flush().expect("flush should succeed");

    let read_back = built.device.read_at(512, 4).expect("read should succeed");
    assert_eq!(read_back, vec![1, 2, 3, 4]);
}

#[test]
fn builder_rejects_mismatched_initial_data_length() {
    let err = MemoryDeviceBuilder::new()
        .block_size(1024)
        .block_count(2)
        .initial_data(vec![0; 3])
        .build()
        .expect_err("builder should reject mismatched initialization");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

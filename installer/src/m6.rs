use std::path::Path;
/// Installer for the M6 hotspot.
///
/// Installation process:
/// 1. ADB should be enabled but non root
/// 2. Switch adb to root
/// 3. Use ADB to install rayhunter files
/// 4. Modify startup script to launch rayhunter on boot
use std::time::Duration;

use adb_client::{ADBDeviceExt, ADBUSBDevice, RustADBError};
use anyhow::{Result, anyhow};
use md5::compute as md5_compute;
use tokio::time::sleep;

use crate::M6Args as Args;
use crate::output::{print, println};

pub async fn install(Args { admin_ip }: Args) -> Result<()> {
    run_install(admin_ip).await
}

async fn run_install(admin_ip: String) -> Result<()> {
    print!("Waiting for ADB connection... ");
    let mut adb_device_init = wait_for_adb().await?;    
    println!("ok");

    print!("Activating ADB Root... ");
    adb_device_init.shell_command(&["sh", "-c", "setprop service.adb.root 1; busybox killall adbd"], &mut buf)?;
    println!("ok");

    print!("Installing rayhunter files... ");
    let mut adb_device = wait_for_adb().await?;
    install_rayhunter_files(&mut adb_device).await?;
    println!("ok");

    print!("Modifying startup script... ");
    modify_startup_script(&mut adb_device).await?;
    println!("ok");

    print!("Rebooting the device... ");
    let _ = adb_device.reboot(adb_client::RebootType::System);
    println!("ok");

    println!("Installation complete!");
    println!("Please wait for the device to reboot (light will turn green)");
    println!("Then access rayhunter at: http://{admin_ip}:8080");

    Ok(())
}

async fn wait_for_adb() -> Result<ADBUSBDevice> {
    const MAX_ATTEMPTS: u32 = 30; // 30 seconds
    let mut attempts = 0;

    // Wait a bit for the reboot to start
    sleep(Duration::from_secs(10)).await;

    loop {
        if attempts >= MAX_ATTEMPTS {
            anyhow::bail!("Timeout waiting for ADB connection after USB debug activation");
        }

        // M6 USB vendor and product IDs.
        // TODO: Research if other variants use different IDs.
        match ADBUSBDevice::new(0x05c6, 0x9024) {
            Ok(mut device) => {
                // Test ADB connection
                if test_adb_connection(&mut device).await.is_ok() {
                    return Ok(device);
                }
            }
            Err(RustADBError::DeviceNotFound(_)) => {
                // Device not ready yet, continue waiting
            }
            Err(e) => {
                anyhow::bail!("ADB connection error: {}", e);
            }
        }

        sleep(Duration::from_secs(1)).await;
        attempts += 1;
    }
}

async fn test_adb_connection(adb_device: &mut ADBUSBDevice) -> Result<()> {
    let mut buf = Vec::<u8>::new();
    adb_device.shell_command(&["echo", "test"], &mut buf)?;
    let output = String::from_utf8_lossy(&buf);
    if output.contains("test") {
        Ok(())
    } else {
        anyhow::bail!("ADB connection test failed")
    }
}

async fn install_rayhunter_files(adb_device: &mut ADBUSBDevice) -> Result<()> {
    // Create rayhunter directory
    let mut buf = Vec::<u8>::new();
    adb_device.shell_command(&["mkdir", "-p", "/data/rayhunter"], &mut buf)?;

    // Remount system as writable
    adb_device.shell_command(&["mount", "-o", "remount,rw", "/system"], &mut buf)?;

    // Install rayhunter daemon binary with verification
    let rayhunter_daemon_bin = crate::get_file!("FILE_RAYHUNTER_DAEMON");
    install_file(
        adb_device,
        "/data/rayhunter/rayhunter-daemon",
        rayhunter_daemon_bin,
    )?;

    // Install config file
    let config_content = crate::CONFIG_TOML.replace("#device = \"orbic\"", "device = \"m6\"");
    let mut config_data = config_content.as_bytes();
    adb_device.push(&mut config_data, &"/data/rayhunter/config.toml")?;

    // Make daemon executable
    let mut buf = Vec::<u8>::new();
    adb_device.shell_command(
        &["chmod", "755", "/data/rayhunter/rayhunter-daemon"],
        &mut buf,
    )?;

    Ok(())
}

/// Transfer a file to the device's filesystem with adb push.
/// Validates the file sends successfully to /data/local/tmp
/// before overwriting the destination.
fn install_file(adb_device: &mut ADBUSBDevice, dest: &str, payload: &[u8]) -> Result<()> {
    const MAX_RETRIES: u32 = 3;

    let file_name = Path::new(dest)
        .file_name()
        .ok_or_else(|| anyhow!("{dest} does not have a file name"))?
        .to_str()
        .ok_or_else(|| anyhow!("{dest}'s file name is not UTF8"))?
        .to_owned();
    let push_tmp_path = format!("/data/local/tmp/{file_name}");
    let file_hash = md5_compute(payload);

    for attempt in 1..=MAX_RETRIES {
        // Push the file
        let mut payload_copy = payload;
        if let Err(e) = adb_device.push(&mut payload_copy, &push_tmp_path) {
            if attempt == MAX_RETRIES {
                return Err(e.into());
            }
            continue;
        }

        // Verify with md5sum
        let mut buf = Vec::<u8>::new();
        if adb_device
            .shell_command(&["busybox", "md5sum", &push_tmp_path], &mut buf)
            .is_ok()
        {
            let output = String::from_utf8_lossy(&buf);
            if output.contains(&format!("{file_hash:x}")) {
                // Verification successful, move to final destination
                let mut buf = Vec::<u8>::new();
                adb_device.shell_command(&["mv", &push_tmp_path, dest], &mut buf)?;
                println!("ok");
                return Ok(());
            }
        }

        // Verification failed, clean up and retry
        if attempt < MAX_RETRIES {
            println!("MD5 verification failed on attempt {attempt}, retrying...");
            let mut buf = Vec::<u8>::new();
            adb_device
                .shell_command(&["rm", "-f", &push_tmp_path], &mut buf)
                .ok();
        }
    }

    anyhow::bail!("MD5 verification failed for {dest} after {MAX_RETRIES} attempts")
}

async fn modify_startup_script(adb_device: &mut ADBUSBDevice) -> Result<()> {
    // Pull the existing startup script
    let mut script_content = Vec::<u8>::new();
    adb_device.pull(&"/system/etc/init.qcom.post_boot.sh", &mut script_content)?;

    // Convert to string and add our line
    let mut script_str = String::from_utf8_lossy(&script_content).into_owned();

    // Add rayhunter startup line if not already present
    let rayhunter_line = "sleep 180\n/data/rayhunter/rayhunter-daemon /data/rayhunter/config.toml &\n";
    if !script_str.contains("/data/rayhunter/rayhunter-daemon") {
        script_str.push_str(rayhunter_line);
    }

    // Push the modified script back
    let mut modified_script = script_str.as_bytes();
    adb_device.push(&mut modified_script, &"/system/etc/init.qcom.post_boot.sh")?;

    // Make sure it's executable
    let mut buf = Vec::<u8>::new();
    adb_device.shell_command(
        &["chmod", "755", "/system/etc/init.qcom.post_boot.sh"],
        &mut buf,
    )?;

    Ok(())
}

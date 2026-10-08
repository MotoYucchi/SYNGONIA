use libloading::{Library, Symbol};
use std::ffi::CString;
use std::os::raw::{c_char, c_uchar, c_uint, c_void};

type FnSpoutInit = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type FnSpoutSendImage = unsafe extern "C" fn(*mut c_void, *const c_uchar, c_uint, c_uint) -> bool;
type FnSpoutIsInitialized = unsafe extern "C" fn(*mut c_void) -> bool;
type FnSpoutRelease = unsafe extern "C" fn(*mut c_void);

fn main() {
    println!("=== Testing Continuous Spout Sending (60s) ===");
    let lib = unsafe { Library::new("syngonia_spout.dll") }.expect("Failed to load syngonia_spout.dll");

    unsafe {
        let spout_init: Symbol<FnSpoutInit> = lib.get(b"Spout_Init").expect("Spout_Init not found");
        let spout_send_image: Symbol<FnSpoutSendImage> =
            lib.get(b"Spout_SendImage").expect("Spout_SendImage not found");
        let spout_release: Symbol<FnSpoutRelease> =
            lib.get(b"Spout_Release").expect("Spout_Release not found");

        let c_name = CString::new("SYNGONIA").unwrap();
        println!("Initializing Spout sender 'SYNGONIA'...");
        let handle = spout_init(c_name.as_ptr());

        if handle.is_null() {
            eprintln!("Spout_Init returned NULL!");
            return;
        }

        let width = 1920u32;
        let height = 1080u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let colors = [
            (255, 255, 255), // White
            (255, 255, 0),   // Yellow
            (0, 255, 255),   // Cyan
            (0, 255, 0),     // Green
            (255, 0, 255),   // Magenta
            (255, 0, 0),     // Red
            (0, 0, 255),     // Blue
        ];
        let bar_width = width / colors.len() as u32;

        for y in 0..height {
            for x in 0..width {
                let bar_idx = ((x / bar_width) as usize).min(colors.len() - 1);
                let (r, g, b) = colors[bar_idx];
                let idx = ((y * width + x) * 4) as usize;
                pixels[idx] = b;
                pixels[idx + 1] = g;
                pixels[idx + 2] = r;
                pixels[idx + 3] = 255;
            }
        }

        println!("Sending 1800 frames (30 seconds @ 60fps)... Watch OBS!");
        for i in 0..1800 {
            // Animate bottom bar to prove it's live
            let cycle = width - 120;
            let block_pos = (i * 8) % (cycle * 2);
            let block_x = if block_pos < cycle { block_pos } else { cycle * 2 - block_pos };
            
            // Draw moving block on bottom
            for y in (height - 100)..height {
                for x in 0..width {
                    let idx = ((y * width + x) * 4) as usize;
                    if x >= block_x && x < block_x + 120 {
                        pixels[idx] = 255;     // B
                        pixels[idx + 1] = 255; // G
                        pixels[idx + 2] = 0;   // R
                    } else {
                        pixels[idx] = 30;
                        pixels[idx + 1] = 20;
                        pixels[idx + 2] = 20;
                    }
                }
            }

            spout_send_image(handle, pixels.as_ptr(), width, height);
            std::thread::sleep(std::time::Duration::from_millis(16));
            if i % 300 == 0 {
                println!("Sent {} frames...", i);
            }
        }

        println!("Releasing Spout...");
        spout_release(handle);
        println!("Finished!");
    }
}

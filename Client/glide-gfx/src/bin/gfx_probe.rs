#[cfg(feature = "vulkan")]
fn main() {
    match unsafe { glide_gfx::vulkan::VulkanBackend::new() } {
        Ok(backend) => {
            println!("GLIDE_GFX_BACKEND=vulkan");
            println!("GLIDE_GFX_DEVICE={}", unsafe { backend.device_name() });
            println!("GLIDE_GFX_QUEUE_FAMILY={}", backend.queue_family());
            println!("GLIDE_GFX_DEVICE_TYPE={:?}", unsafe { backend.device_type() });

            let mut backend = backend;
            let buffer = glide_gfx::ResourceId::from_raw(1);
            let texture = glide_gfx::ResourceId::from_raw(2);
            let texture_data = vec![
                255u8, 0, 0, 255, 0, 255, 0, 255,
                0, 0, 255, 255, 255, 255, 255, 255,
            ];
            let commands = [
                glide_gfx::Command::CreateBuffer {
                    id: buffer, usage: glide_gfx::BufferUsage::Vertex, size: 16,
                },
                glide_gfx::Command::UploadBuffer {
                    id: buffer, offset: 0,
                    data: vec![1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],
                },
                glide_gfx::Command::CreateTexture {
                    id: texture, width: 2, height: 2,
                    format: glide_gfx::TextureFormat::Rgba8,
                },
                glide_gfx::Command::UploadTexture {
                    id: texture, x: 0, y: 0, width: 2, height: 2, data: texture_data,
                },
            ];

            match unsafe { backend.submit_commands(&commands) } {
                Ok(()) => {
                    println!("GLIDE_GFX_COMMAND_SUBMIT=ok");
                    println!("GLIDE_GFX_BUFFER_UPLOAD=ok");
                    println!("GLIDE_GFX_TEXTURE_UPLOAD=ok");
                }
                Err(error) => {
                    println!("GLIDE_GFX_COMMAND_SUBMIT=error");
                    println!("GLIDE_GFX_COMMAND_ERROR={error}");
                    std::process::exit(3);
                }
            }

            match unsafe { backend.render_test_frame(1280, 720) } {
                Ok(()) => println!("GLIDE_GFX_TRIANGLE_RENDER=ok"),
                Err(error) => {
                    println!("GLIDE_GFX_TRIANGLE_RENDER=error");
                    println!("GLIDE_GFX_TRIANGLE_ERROR={error}");
                    std::process::exit(4);
                }
            }
        }
        Err(error) => {
            println!("GLIDE_GFX_BACKEND=unavailable");
            println!("GLIDE_GFX_ERROR={error}");
            std::process::exit(2);
        }
    }
}

#[cfg(not(feature = "vulkan"))]
fn main() {
    println!("GLIDE_GFX_BACKEND=disabled");
    println!("Enable the 'vulkan' Cargo feature to probe the accelerated backend.");
}

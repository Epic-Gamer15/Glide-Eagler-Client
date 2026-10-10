use glide_gfx::{BlendMode, Command, CommandBuffer, ResourceId};

fn main() {
    let mut commands = CommandBuffer::new();

    for i in 0..100_000u32 {
        let texture = ResourceId::from_raw(i % 32);
        commands.push(Command::BindTexture { slot: 0, texture });
        commands.push(Command::SetBlend(BlendMode::Alpha));
        commands.push(Command::SetBlend(BlendMode::Alpha));
        commands.push(Command::Draw {
            primitive: glide_gfx::Primitive::Triangles,
            first: 0,
            count: 6,
        });
    }

    let before = commands.len();
    let start = std::time::Instant::now();
    commands.optimize();
    let elapsed = start.elapsed();

    println!("GLIDE_IR_COMMANDS_BEFORE={before}");
    println!("GLIDE_IR_COMMANDS_AFTER={}", commands.len());
    println!("GLIDE_IR_OPTIMIZE_US={}", elapsed.as_micros());
}

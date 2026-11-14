// The chat in the world: each message floats in front of where you looked when it came,
// rises slowly and fades out, facing the camera.
fn take_your_stream_chat_world(
    game: &crate::game::Game<'_>,
    camera: [f64; 3],
    primitives: &mut horizon_client::world_primitives::WorldPrimitives,
) {
    use crate::game::take_your_stream_chat as m;
    use horizon_client::world_primitives::PrimitiveShape;
    if !game.native_on(m::ID) {
        return;
    }
    m::ensure_loaded(|key| game.native_setting(m::ID, key));
    if !m::mode().world() {
        return;
    }
    let Some(player) = game.native_player() else {
        return;
    };
    for t in m::world_texts(camera, player.yaw) {
        primitives.push(PrimitiveShape::Text {
            at: t.at,
            text: t.text,
            color: t.color,
            scale: 1.2,
            scale_with_distance: false,
            background: true,
        });
    }
}

//! Small valid image fixtures shared by the code-output integration tests.

pub fn png(blue: bool) -> Vec<u8> {
    use base64::Engine as _;
    let encoded = if blue {
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAJklEQVR42u3NsQkAAAjAsP7/tF7hIASyp5pjAoFAIBAIBAKB4EmwOkv8Lom8x/sAAAAASUVORK5CYII="
    } else {
        "iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAIAAAD8GO2jAAAAKElEQVR4nO3NsQ0AAAzCMP5/un0CNkuZ41wybXsHAAAAAAAAAAAAxR4yw/wuPL6QkAAAAABJRU5ErkJggg=="
    };
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("valid PNG fixture")
}

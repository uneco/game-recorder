# game-recorder

A self-contained, out-of-process Windows game recorder (~500 KB single executable, no external runtime or FFmpeg required).

- **Video**: Windows Graphics Capture (WGC) + D3D11 GPU textures + Media Foundation `IMFSinkWriter` (hardware H.264, 30 FPS, 8 Mbps, scaled to fit inside 1920x1080 with even dimensions, no CPU readback)
- **Audio**: WASAPI process loopback (captures only the target process and its children) mixed at 55% with the default communications microphone at 100%, encoded as 48 kHz stereo 160 kbps AAC
- **Isolation**: Communicates via files in a per-session `--control-dir`, so the host game can exit cleanly while the recorder finalizes the MP4 independently

## Requirements

- Windows 11 (or Windows build >= 20348 supporting WASAPI process loopback)
- GPU driver exposing a hardware H.264 Media Foundation encoder
- Default communications microphone with desktop-app privacy permission enabled

## Usage

```text
game-recorder.exe --hwnd <decimal> --pid <process-id> --output <absolute.mp4> --control-dir <absolute-directory>
```

- Use a fresh `--control-dir` and a non-existing `--output` path for each recording.
- Launch asynchronously without a console window.
- Poll `<control-dir>/status.json` for state updates:
  ```json
  { "state": "starting|recording|stopping|completed|error", "output": "...", "message": "..." }
  ```
- Create an empty `<control-dir>/stop` file to finish recording cleanly. Never kill the process directly.
- Output is written to `<stem>.partial.mp4` and atomically renamed to `--output` only after successful MP4 finalization.
- Window resize, window close, or target process exit automatically triggers clean finalization.

## Building from Source

Requires Rust >= 1.87 on Windows:

```powershell
cargo test --locked
cargo build --release --locked
```

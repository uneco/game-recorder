---
name: godot-windows-recorder
description: Godot 4 (Windows) プロジェクトに、ビルド済みの軽量ネイティブ録画ヘルパー（game-recorder.exe / 約500KB）を使ったローカルMP4録画機能を追加するスキル。RustツールチェーンやFFmpeg不要で、ゲーム音（プロセスループバック）とマイク音声をミックスしてローカルにMP4保存する一式を実装する。
---

# Godot 4 Windows ローカル録画機能 実装スキル

Godot 4 のゲームクライアントに、別プロセスで動作するビルド済みネイティブ録画ヘルパー（[`uneco/game-recorder`](https://github.com/uneco/game-recorder)、約 500 KB・外部ランタイム/FFmpeg/Rust 不要）を使った MP4 録画機能を追加する手順書。

- **映像**: Windows Graphics Capture + D3D11 GPU テクスチャ + Media Foundation ハードウェア H.264（30 FPS / 8 Mbps / 最大 1080p にアスペクト比維持で縮小 / CPU 読み戻しなし）
- **音声**: WASAPI プロセスループバック（対象ゲームプロセスとその子プロセスのみ、他アプリの音を拾わない）+ デフォルト通信マイクを 48 kHz ステレオでミックス（160 kbps AAC）
- **配布性**: エクスポート済み PCK 内に `game-recorder.exe` を同梱し、初回起動時に `user://rec/` へ展開して実行できる

## 動作要件

- **OS**: Windows 11（またはプロセスオーディオキャプチャ対応の Windows build 20348 以降）
- **GPU**: ハードウェア H.264 Media Foundation エンコーダーを提供する GPU ドライバ
- **マイク**: Windows の既定の通信マイク（デスクトップアプリのマイクアクセス許可が必要）
- **Godot**: Godot 4.x

---

## ステップ 1: 実装前にユーザーへ確認する事項

ファイルを作成する前に、以下をユーザーへ確認する（未指定の項目は括弧内のデフォルトを提案する）。

1. **録画ファイル（MP4）の保存先とファイル名**
   - 保存先フォルダ（例: `OS.get_system_dir(OS.SYSTEM_DIR_MOVIES).path_join("<AppName>")` / `user://recordings` / 任意パス）
   - ファイル名のプレフィックス（例: `<AppName>` → `<AppName>-<timestamp>.mp4`）
2. **プロジェクト内のファイル配置先**
   - バイナリ `game-recorder.exe` の配置先（例: `addons/game_recorder/bin/game-recorder.exe`）
   - GDScript の配置先（例: `addons/game_recorder/` または `scripts/recording/`）
3. **UI の組み込み方法**
   - 独立したオーバーレイ UI（`CanvasLayer` に録画ボタンと保存通知を表示）を追加するか
   - 既存の HUD やショートカットキーに `GameRecorder` ノードの `toggle()` を接続するか
   - コントローラー（`game_recorder.gd`）だけ配置するか
4. **通知メッセージの言語**
   - 日本語 / 英語 / `TranslationServer`（`.po`）対応

---

## ステップ 2: `game-recorder.exe` のダウンロードと配置

GitHub Releases からビルド済みバイナリを取得し、ステップ 1 で決めた配置先（例: `addons/game_recorder/bin/game-recorder.exe`）へ保存する。

```powershell
# PowerShell（リポジトリルートから実行）
$dest = "addons/game_recorder/bin/game-recorder.exe"
New-Item -ItemType Directory -Force -Path (Split-Path $dest) | Out-Null
Invoke-WebRequest -Uri "https://github.com/uneco/game-recorder/releases/latest/download/game-recorder.exe" -OutFile $dest
& $dest --help
```

`gh` CLI を使う場合：

```powershell
gh release download --repo uneco/game-recorder --pattern "game-recorder.exe" --dir addons/game_recorder/bin --clobber
```

> **エクスポート時の注意**: Godot で Windows 向けエクスポート（PCK 同梱）を行う場合、エクスポートプリセットの「非リソースファイルのエクスポートフィルタ（`include_filter`）」に `*.exe`（または `addons/game_recorder/bin/game-recorder.exe`）を追加すること。未設定だと PCK に `.exe` が含まれない。

---

## ステップ 3: ヘルパーのプロトコルと設計上の注意点

Godot 側のコードをカスタマイズする際も、以下の仕様は必ず守ること。

- **起動引数**: `game-recorder.exe --hwnd <decimal> --pid <game-pid> --output <absolute.mp4> --control-dir <absolute-directory>`
  - `--output` は未存在の絶対パス、毎回新しい `--control-dir` を渡す。
- **ファイルによる状態通知と停止**:
  - ヘルパーは `<control-dir>/status.json` に `{"state": "starting|recording|stopping|completed|error", "output": "...", "message": "..."}` を原子的に書き出す。Godot は 0.25 秒間隔でポーリングする。
  - 録画を止めるときは**プロセスを強制終了せず**、`<control-dir>/stop`（空ファイル）を作成する。ヘルパーは MP4 の `Finalize()` 後に `<stem>.partial.mp4` を `--output` へリネームして `completed` を書く。
  - 親プロセス終了・ウィンドウ破棄・リサイズもヘルパー側で監視しているため、ゲーム終了時は `_exit_tree()` で `stop()` を呼ぶだけでよい。
- **Windows パス長制限（MAX_PATH）対策**:
  - Windows 上の Godot の `OS.create_process` は **258 文字（コードポイント）** または **259 UTF-16 コードユニット** を超えると起動に失敗する。またヘルパーの `status.json` も 259 UTF-16 ユニット上限を受ける。
  - そのため PCK 展開先とコントロールディレクトリは `user://rec` のように極力短く保ち、起動前にパス長チェックを行うこと。

---

## ステップ 4: Godot コントローラー (`game_recorder.gd`) の作成

ユーザー指定の GDScript 配置先に `game_recorder.gd` を作成する。
ステップ 1 の回答に合わせて以下を書き換える：
- `HELPER`: `game-recorder.exe` の `res://` パス
- `FILE_PREFIX`: MP4 ファイル名のプレフィックス
- `recordings_folder()`: 保存先ディレクトリ
- `MESSAGES`: 通知文言

```gdscript
class_name GameRecorder
extends Node
## Windowsネイティブ録画ヘルパーの起動・停止・状態監視を行うコントローラー。
## 映像取得・音声キャプチャ・エンコードはすべて別プロセスで行われる。

signal changed(state: String)
signal saved(path: String)
signal failed(message: String)

const HELPER := "res://addons/game_recorder/bin/game-recorder.exe"
const FILE_PREFIX := "Recording"
## エクスポート版PCKからの展開先およびコントロールフォルダの親。
## Windowsのパス長制限を避けるため短く保つ。
const HELPERS := "user://rec"
## Godot 4 on Windows の OS.create_process / ヘルパー status.json のパス長上限
const HELPER_PATH_LIMIT := 258
const HELPER_PATH_UNITS_LIMIT := 259
const STATUS_PATH_UNITS_LIMIT := 259
const POLL_SECONDS := 0.25

const MESSAGES := {
	"start": "録画を開始（ゲーム音とマイク音声を含みます）",
	"stop": "録画を停止",
	"starting": "録画を開始しています…",
	"stopping": "録画を保存しています…",
	"saved": "録画を保存しました: {path}",
	"failed": "録画に失敗しました",
	"unsupported": "録画機能はWindowsでのみ利用できます",
	"unavailable": "録画機能を準備できませんでした",
	"path_too_long": "保存先フォルダのパスが長すぎるため、録画を開始できませんでした",
}

var state := "idle"
var output := ""
var last_saved := ""
var _pid := -1
var _control := ""
var _since_poll := 0.0

func _ready() -> void:
	process_mode = Node.PROCESS_MODE_ALWAYS
	set_process(false)

func available() -> bool:
	return OS.get_name() == "Windows" and DisplayServer.get_name() != "headless"

func active() -> bool:
	return state in ["starting", "recording", "stopping"]

func toggle() -> void:
	if state in ["starting", "recording"]:
		stop()
	elif not active():
		start()

func start() -> void:
	if active():
		return
	if not available():
		_fail("unsupported")
		return
	var executable := _helper_path()
	if executable.is_empty():
		_fail("unavailable")
		return
	var folder := recordings_folder()
	if DirAccess.make_dir_recursive_absolute(folder) != OK:
		_fail("failed")
		return
	var stamp := Time.get_datetime_string_from_system().replace(":", "-")
	output = folder.path_join("%s-%s-%s.mp4" % [FILE_PREFIX, stamp, Time.get_ticks_usec()])
	_control = control_folder()
	var status := _control.path_join("status.json")
	if executable.length() > HELPER_PATH_LIMIT \
			or _utf16_units(executable) > HELPER_PATH_UNITS_LIMIT \
			or _utf16_units(status) > STATUS_PATH_UNITS_LIMIT:
		push_warning("Game recording: path too long (helper=%s, status=%s)" % [executable, status])
		_control = ""
		_fail("path_too_long")
		return
	if DirAccess.make_dir_recursive_absolute(_control) != OK:
		_fail("failed")
		return
	var hwnd := DisplayServer.window_get_native_handle(
		DisplayServer.WINDOW_HANDLE,
		get_window().get_window_id()
	)
	_set_state("starting")
	_pid = _launch(executable, PackedStringArray([
		"--hwnd", str(hwnd),
		"--pid", str(OS.get_process_id()),
		"--output", output,
		"--control-dir", _control,
	]))
	if _pid <= 0:
		push_warning("Game recording: failed to launch helper at %s" % executable)
		_fail("failed")
		_cleanup_control()
		return
	_since_poll = 0.0
	set_process(true)

func stop() -> void:
	if state not in ["starting", "recording"]:
		return
	var marker := FileAccess.open(_control.path_join("stop"), FileAccess.WRITE)
	if marker == null:
		failed.emit(MESSAGES["failed"])
		return
	marker.close()
	_set_state("stopping")

func _exit_tree() -> void:
	# ゲーム終了時もヘルパーが単独でMP4のFinalizeを完了できるよう停止要求だけ出す。
	stop()

func _process(delta: float) -> void:
	_since_poll += delta
	if _since_poll < POLL_SECONDS:
		return
	_since_poll = 0.0
	poll()

func poll() -> void:
	if not active():
		return
	var update := _read_status()
	var next := str(update.get("state", ""))
	if next == "completed":
		if not FileAccess.file_exists(output):
			_fail("failed")
		else:
			last_saved = output
			_set_state("idle")
			saved.emit(output)
		_finish()
	elif next == "error":
		push_warning("Game recording: " + str(update.get("message", "unknown helper error")))
		_fail("failed")
		_finish()
	elif not _is_running():
		var status := _control.path_join("status.json")
		if not FileAccess.file_exists(status):
			push_warning("Game recording: helper exited without writing status at %s" % status)
		_fail("failed")
		_finish()
	elif next == "recording" and state == "starting":
		_set_state("recording")
	elif next == "stopping":
		_set_state("stopping")

## 録画ファイルの保存先ディレクトリ。プロジェクトに合わせて変更する。
func recordings_folder() -> String:
	return OS.get_system_dir(OS.SYSTEM_DIR_MOVIES).path_join(
		ProjectSettings.get_setting("application/config/name", "GodotGame")
	)

func control_folder() -> String:
	return ProjectSettings.globalize_path(HELPERS.path_join("s%s-%s" % [OS.get_process_id(), Time.get_ticks_usec()]))

func tooltip() -> String:
	if state == "starting": return MESSAGES["starting"]
	if state == "recording": return MESSAGES["stop"]
	if state == "stopping": return MESSAGES["stopping"]
	return MESSAGES["start"]

func _utf16_units(text: String) -> int:
	return text.to_utf16_buffer().size() / 2

func _launch(executable: String, args: PackedStringArray) -> int:
	return OS.create_process(executable, args, false)

func _is_running() -> bool:
	return _pid > 0 and OS.is_process_running(_pid)

func _read_status() -> Dictionary:
	var path := _control.path_join("status.json")
	if not FileAccess.file_exists(path):
		return {}
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(path))
	return parsed if parsed is Dictionary else {}

func _helper_path() -> String:
	# エディタ実行時は res:// から直接参照できる
	var direct := ProjectSettings.globalize_path(HELPER)
	var directory := DirAccess.open(direct.get_base_dir())
	if directory != null and directory.file_exists(direct.get_file()):
		return direct
	# エクスポートPCK実行時は SHA-256 先頭16文字の名前で user://rec/ へ原子的展開する
	if not FileAccess.file_exists(HELPER):
		return ""
	var digest := FileAccess.get_sha256(HELPER)
	if digest.is_empty():
		return ""
	var destination := ProjectSettings.globalize_path(HELPERS.path_join(digest.left(16) + ".exe"))
	if FileAccess.file_exists(destination) and FileAccess.get_sha256(destination) == digest:
		return destination
	if DirAccess.make_dir_recursive_absolute(destination.get_base_dir()) != OK:
		return ""
	var bytes := FileAccess.get_file_as_bytes(HELPER)
	var temporary := destination + ".%s.tmp" % OS.get_process_id()
	var file := FileAccess.open(temporary, FileAccess.WRITE)
	if file == null:
		return ""
	file.store_buffer(bytes)
	var ok := file.get_error() == OK
	file.close()
	if not ok or FileAccess.get_sha256(temporary) != digest:
		DirAccess.remove_absolute(temporary)
		return ""
	if DirAccess.rename_absolute(temporary, destination) != OK:
		DirAccess.remove_absolute(temporary)
		if not FileAccess.file_exists(destination) or FileAccess.get_sha256(destination) != digest:
			return ""
	return destination

func _fail(key: String) -> void:
	_set_state("error")
	failed.emit(MESSAGES[key])

func _set_state(value: String) -> void:
	if state == value:
		return
	state = value
	changed.emit(state)

func _finish() -> void:
	set_process(false)
	_pid = -1
	_cleanup_control()

func _cleanup_control() -> void:
	if _control.is_empty():
		return
	for filename: String in ["stop", "status.json", "status.tmp"]:
		var path := _control.path_join(filename)
		if FileAccess.file_exists(path):
			DirAccess.remove_absolute(path)
	DirAccess.remove_absolute(_control)
	_control = ""
```

---

## ステップ 5: オーバーレイ UI の追加（任意）

独立したオーバーレイ UI を希望された場合は、以下の 2 ファイルを `game_recorder.gd` と同じディレクトリに配置し、Autoload（シングルトン）に登録するかメインシーンへ `add_child()` する。

### `recording_icon.gd`

```gdscript
extends Control

var recording := false:
	set(value):
		recording = value
		queue_redraw()
var busy := false:
	set(value):
		busy = value
		queue_redraw()

func _ready() -> void:
	mouse_filter = Control.MOUSE_FILTER_IGNORE
	custom_minimum_size = Vector2(32, 32)
	size = custom_minimum_size

func _draw() -> void:
	var ink := Color("edb85e") if busy else Color("f17470")
	var box := StyleBoxFlat.new()
	box.bg_color = Color(0.03, 0.04, 0.04, 0.65)
	box.border_color = Color(0.87, 0.88, 0.82, 0.8)
	box.set_border_width_all(1)
	box.set_corner_radius_all(4)
	box.anti_aliasing = false
	draw_style_box(box, Rect2(5, 5, 22, 22))
	if recording or busy:
		draw_rect(Rect2(11, 11, 10, 10), ink)
	else:
		for row in 12:
			var inset: int = [4, 2, 1, 1, 0, 0, 0, 0, 1, 1, 2, 4][row]
			draw_rect(Rect2(10 + inset, 10 + row, 12 - inset * 2, 1), ink)
```

### `recording_overlay.gd`

```gdscript
extends CanvasLayer

const Recorder := preload("./game_recorder.gd")
const RecordingIcon := preload("./recording_icon.gd")

var recorder: GameRecorder
var _button: Button
var _icon: Control
var _notice: Label
var _notice_timer: Timer

func _ready() -> void:
	name = "RecordingOverlay"
	layer = 100
	process_mode = Node.PROCESS_MODE_ALWAYS
	if recorder == null:
		recorder = Recorder.new()
	recorder.name = "Recorder"
	add_child(recorder)

	var surface := Control.new()
	surface.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(surface)
	surface.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)

	_button = Button.new()
	_button.focus_mode = Control.FOCUS_NONE
	_button.mouse_filter = Control.MOUSE_FILTER_STOP
	_button.add_theme_font_size_override("font_size", 16)
	_button.add_theme_stylebox_override("normal", _box(Color("20252eee")))
	_button.add_theme_stylebox_override("hover", _box(Color("384351")))
	_button.add_theme_stylebox_override("pressed", _box(Color("101720")))
	_button.add_theme_stylebox_override("disabled", _box(Color("20252eee")))
	surface.add_child(_button)
	_button.set_anchors_and_offsets_preset(Control.PRESET_CENTER_TOP)
	_button.offset_left = -80
	_button.offset_right = 80
	_button.offset_top = 8
	_button.offset_bottom = 48
	_button.pressed.connect(recorder.toggle)

	_icon = RecordingIcon.new()
	_icon.position = Vector2(4, 4)
	_button.add_child(_icon)

	_notice = Label.new()
	_notice.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_notice.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_notice.add_theme_font_size_override("font_size", 16)
	_notice.add_theme_color_override("font_color", Color.WHITE)
	_notice.add_theme_color_override("font_shadow_color", Color.BLACK)
	_notice.add_theme_constant_override("shadow_offset_x", 1)
	_notice.add_theme_constant_override("shadow_offset_y", 1)
	surface.add_child(_notice)
	_notice.anchor_right = 0.7
	_notice.anchor_top = 1.0
	_notice.anchor_bottom = 1.0
	_notice.offset_left = 12
	_notice.offset_top = -112
	_notice.offset_bottom = -12
	_notice.hide()

	_notice_timer = Timer.new()
	_notice_timer.one_shot = true
	_notice_timer.wait_time = 8.0
	add_child(_notice_timer)
	_notice_timer.timeout.connect(_notice.hide)

	recorder.changed.connect(func(_state: String): _show_state())
	recorder.saved.connect(func(path: String): _show_notice(Recorder.MESSAGES["saved"].format({"path": path})))
	recorder.failed.connect(_show_notice)
	_show_state()

func _box(color: Color) -> StyleBoxFlat:
	var box := StyleBoxFlat.new()
	box.bg_color = color
	box.border_color = Color("a8b1bb")
	box.set_border_width_all(1)
	box.set_corner_radius_all(5)
	box.content_margin_left = 36
	box.content_margin_right = 8
	return box

func _show_state() -> void:
	visible = recorder.available()
	_button.disabled = recorder.state == "stopping"
	_button.tooltip_text = recorder.tooltip()
	var label := "REC"
	if recorder.state == "starting": label = "STARTING"
	elif recorder.state == "recording": label = "STOP"
	elif recorder.state == "stopping": label = "SAVING"
	_button.text = label
	_icon.recording = recorder.state == "recording"
	_icon.busy = recorder.state in ["starting", "stopping"]

func _show_notice(message: String) -> void:
	_notice.text = message
	_notice.show()
	_notice_timer.start()
```

---

## ステップ 6: 動作確認

1. **バイナリ単体チェック**
   ```powershell
   addons/game_recorder/bin/game-recorder.exe --help
   ```
2. **実機録画チェック**
   - Godot エディタからゲームを起動し、録画開始 → 数秒操作（音声あり）→ 録画停止を行う。
   - 保存先フォルダに `.mp4` が生成され、映像・ゲーム音・マイク音が再生できることを確認する。
   - 録画中にウィンドウをリサイズした場合、その時点までの録画が自動保存されることを確認する。
   - 録画中にゲームウィンドウを閉じた場合、ヘルパーが単独で MP4 をファイナライズして保存することを確認する。

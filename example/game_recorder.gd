class_name GameRecorder
extends Node
## Windowsネイティブ録画ヘルパー（game-recorder.exe）の起動・停止・状態監視を行うコントローラー。
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

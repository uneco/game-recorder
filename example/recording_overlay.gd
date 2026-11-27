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

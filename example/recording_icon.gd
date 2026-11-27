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

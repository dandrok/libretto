# libretto — the `follow` recording (docs/follow.gif).
#
#   docs/record-demo.sh follow    # builds the throwaway library, then records this tape
#
# Same requirements and the same /tmp/lib scratch tree as docs/demo.tape. This one shows the
# read-along: the sentence before in dim, the sentence being spoken with one word underlined,
# the next one dim below it, and the bar underneath.
#
# Sizing: `--width 52` keeps the block at 54 columns counting its indent, but the closing line is
# 62, so the window is sized for 64: ~1130 px plus padding, 1180. A wrapped row is what breaks the
# in-place redraw, and a wrapped closing line looks like a bug in a still. The height has to hold the
# tallest frame: header, six-row wordmark, previous row, up to four rows of the current sentence,
# the next row and the bar — 14 rows at ~31 px — plus the command echo, the stop line and the
# prompt: 19 rows at ~31 px is 590, so 780 also survives the frames where the block is at its
# tallest without scrolling the header off the top.

Output docs/follow.gif

Set Shell bash
Set FontFamily "JetBrainsMono NF, Adwaita Mono, Noto Color Emoji, monospace"
Set FontSize 26
Set LineHeight 1.2
Set Width 1180
Set Height 780
Set Padding 24
Set Theme "Dracula"
Set Framerate 25
Set TypingSpeed 35ms
Set CursorBlink false

Hide
Type "cd /tmp/lib && export XDG_DATA_HOME=$PWD/data XDG_CONFIG_HOME=$PWD/config XDG_CACHE_HOME=$PWD/cache MPV_HOME=$PWD/mpv PATH=$PWD/bin:$PATH; PS1='$ '; clear"
Enter
Sleep 800ms
Show

# Chapter 2 of three: the meter starts about a third across, so the bar, the chapter counter and
# the sentence counter all have something to say, and it reads differently from the 62% in
# docs/demo.gif. Three sentences, because the point is the word moving under the voice.
Type "libretto follow the-art --chapter 2 --sentences 3 --width 52"
Enter
# Three sentences, about six seconds, plus the first frame's build. Paced rather than waited:
# `follow` rewrites the same rows, and a screen wait on its closing line does not match reliably.
Sleep 12s
Sleep 2500ms

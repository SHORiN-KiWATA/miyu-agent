"""按「同步输出」的规矩把 PTY 字节喂给 pyte：一帧画完了才看得见。

全屏 TUI 的每次重绘都裹在 `ESC[?2026h … ESC[?2026l` 里，支持这个模式的终端在
`l` 到来之前不显示这一帧的任何东西。pyte 不认这个模式，来多少画多少；而 PTY 一次
最多读出 4095 字节。09-24 流畅度专项（95cbde16）起一帧合成一次写，大厅开到 40 行
时星空每拍一帧 3.8–5.2KB，读一次正好切在帧中间，测具看到的是画了一半的屏
（session_picker / effort_menu 因此稳定红，而真终端上什么都没少）。

帧外的字节（启动探针、模式切换）照常马上喂：终端对它们也是马上生效的，光标位置
查询更得当场回答。

    feed = SyncFeed(pyte.Stream(screen))
    feed.feed(chunk)          # 每读到一块就喂；没画完的那一帧先扣着
"""

import codecs

FRAME_START = b"\x1b[?2026h"
FRAME_END = b"\x1b[?2026l"


def visible_end(buffer):
    """`buffer` 开头有多少字节此刻已经显示出来了。"""
    start = buffer.rfind(FRAME_START)
    if start >= 0 and buffer.find(FRAME_END, start) < 0:
        return start
    # 末尾可能是半截帧头，下一次读才补全：先扣着，免得这一帧被当成帧外字节喂掉。
    for keep in range(len(FRAME_START) - 1, 0, -1):
        if buffer.endswith(FRAME_START[:keep]):
            return len(buffer) - keep
    return len(buffer)


class SyncFeed:
    """把字节按帧喂给一个 `pyte.Stream`。"""

    def __init__(self, stream):
        self.stream = stream
        self.decoder = codecs.getincrementaldecoder("utf-8")(errors="replace")
        self.held = bytearray()

    def feed(self, chunk):
        self.held.extend(chunk)
        cut = visible_end(self.held)
        if not cut:
            return
        text = self.decoder.decode(bytes(self.held[:cut]))
        del self.held[:cut]
        if text:
            self.stream.feed(text)

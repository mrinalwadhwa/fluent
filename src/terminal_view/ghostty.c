// Keep the evolving upstream ABI behind this small, owned C boundary.
#include <ghostty/vt.h>
#include <stdlib.h>
#include <string.h>

typedef struct {
    uint8_t text[128];
    uint32_t len;
    uint8_t foreground[3], background[3];
    uint16_t flags;
    uint8_t width;
} FluentCell;

typedef struct {
    GhosttyTerminal terminal;
    GhosttyRenderState render;
    GhosttyRenderStateRowIterator rows;
    GhosttyRenderStateRowCells cells;
    uint8_t replies[4096];
    size_t reply_len;
    bool failed;
} FluentVt;

static void reply(GhosttyTerminal terminal, void *userdata, const uint8_t *bytes, size_t len) {
    (void)terminal;
    FluentVt *vt = userdata;
    if (len > sizeof(vt->replies) - vt->reply_len) { vt->failed = true; return; }
    memcpy(vt->replies + vt->reply_len, bytes, len);
    vt->reply_len += len;
}

void fluent_vt_free(FluentVt *vt) {
    if (!vt) return;
    ghostty_render_state_row_cells_free(vt->cells);
    ghostty_render_state_row_iterator_free(vt->rows);
    ghostty_render_state_free(vt->render);
    ghostty_terminal_free(vt->terminal);
    free(vt);
}

FluentVt *fluent_vt_new(uint16_t columns, uint16_t rows) {
    FluentVt *vt = calloc(1, sizeof(*vt));
    if (!vt) return NULL;
    if (ghostty_terminal_new(NULL, &vt->terminal, columns, rows) != GHOSTTY_SUCCESS ||
        ghostty_render_state_new(NULL, &vt->render) != GHOSTTY_SUCCESS ||
        ghostty_render_state_row_iterator_new(NULL, &vt->rows) != GHOSTTY_SUCCESS ||
        ghostty_render_state_row_cells_new(NULL, &vt->cells) != GHOSTTY_SUCCESS) {
        fluent_vt_free(vt); return NULL;
    }
    size_t zero = 0;
    // Retain only the viewport; tmux owns scrollback. Never open image files,
    // manipulate the host clipboard, execute commands, or change host titles.
    ghostty_terminal_set(vt->terminal, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &zero);
    ghostty_terminal_set(vt->terminal, GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &zero);
    ghostty_terminal_set(vt->terminal, GHOSTTY_TERMINAL_OPT_CLIPBOARD_WRITE_MAX_BYTES, &zero);
    ghostty_terminal_set(vt->terminal, GHOSTTY_TERMINAL_OPT_USERDATA, vt);
    ghostty_terminal_set(vt->terminal, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (void *)reply);
    return vt;
}

void fluent_vt_write(FluentVt *vt, const uint8_t *bytes, size_t len) {
    ghostty_terminal_vt_write(vt->terminal, bytes, len);
}

size_t fluent_vt_replies(FluentVt *vt, uint8_t *out) {
    size_t len = vt->reply_len;
    memcpy(out, vt->replies, len);
    vt->reply_len = 0;
    return len;
}

int fluent_vt_snapshot(FluentVt *vt, FluentCell *out, size_t capacity, uint16_t *cursor) {
    if (vt->failed || ghostty_render_state_update(vt->render, vt->terminal) != GHOSTTY_SUCCESS) return -1;
    GhosttyRenderStateColors colors = GHOSTTY_INIT_SIZED(GhosttyRenderStateColors);
    GhosttyRenderStateCursor position = GHOSTTY_INIT_SIZED(GhosttyRenderStateCursor);
    if (ghostty_render_state_get(vt->render, GHOSTTY_RENDER_STATE_DATA_COLORS, &colors) != GHOSTTY_SUCCESS ||
        ghostty_render_state_get(vt->render, GHOSTTY_RENDER_STATE_DATA_CURSOR, &position) != GHOSTTY_SUCCESS ||
        ghostty_render_state_get(vt->render, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &vt->rows) != GHOSTTY_SUCCESS) return -1;
    cursor[0] = position.viewport_x;
    cursor[1] = position.viewport_y;
    cursor[2] = position.visible && position.viewport_has_value;
    size_t index = 0;
    while (ghostty_render_state_row_iterator_next(vt->rows)) {
        if (ghostty_render_state_row_get(vt->rows, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &vt->cells) != GHOSTTY_SUCCESS) return -1;
        while (ghostty_render_state_row_cells_next(vt->cells)) {
            if (index >= capacity) return -1;
            FluentCell *cell = &out[index++];
            memset(cell, 0, sizeof(*cell));
            GhosttyBuffer text = {.ptr = cell->text, .cap = sizeof(cell->text), .len = 0};
            if (ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_UTF8, &text) != GHOSTTY_SUCCESS) return -1;
            cell->len = (uint32_t)text.len;
            GhosttyStyle style = GHOSTTY_INIT_SIZED(GhosttyStyle);
            GhosttyCell raw;
            GhosttyCellWide wide;
            if (ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &style) != GHOSTTY_SUCCESS ||
                ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_RAW, &raw) != GHOSTTY_SUCCESS ||
                ghostty_cell_get(raw, GHOSTTY_CELL_DATA_WIDE, &wide) != GHOSTTY_SUCCESS) return -1;
            cell->width = wide == GHOSTTY_CELL_WIDE_WIDE ? 2 : wide == GHOSTTY_CELL_WIDE_SPACER_TAIL ? 0 : 1;
            GhosttyColorRgb fg = colors.foreground, bg = colors.background;
            ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, &fg);
            ghostty_render_state_row_cells_get(vt->cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR, &bg);
            memcpy(cell->foreground, &fg, 3); memcpy(cell->background, &bg, 3);
            cell->flags = style.bold | (style.italic << 1) | (style.faint << 2) |
                (style.blink << 3) | (style.inverse << 4) | (style.invisible << 5) |
                (style.strikethrough << 6) | ((style.underline != 0) << 7);
        }
    }
    return index == capacity ? 0 : -1;
}

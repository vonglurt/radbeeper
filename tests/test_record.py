# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Paul Richeson
"""The terminal emulator behind docs/screenshots.

These matter more than they look. Every image in the README is produced by
replaying a recording through this class, so a bug here does not raise an
exception -- it publishes a picture of something the program never drew. The
scroll-region case below is exactly that: it put the random line five rows up,
on top of the spectrum, and only on a screen busy enough for ncurses to decide
scrolling was cheaper than redrawing.
"""
import os
import sys
import unittest

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                os.pardir, "tools"))
import importlib.machinery
import importlib.util

_loader = importlib.machinery.SourceFileLoader(
    "record", os.path.join(os.path.dirname(os.path.abspath(__file__)),
                           os.pardir, "tools", "record.py"))
_spec = importlib.util.spec_from_loader("record", _loader)
record = importlib.util.module_from_spec(_spec)
sys.modules["record"] = record
_loader.exec_module(record)


def screen(cols=20, rows=6, *chunks):
    s = record.Screen(cols, rows)
    for c in chunks:
        s.feed(c if isinstance(c, bytes) else c.encode())
    return s


class TestScreen(unittest.TestCase):

    def rows(self, s):
        return ["".join(c.ch for c in row).rstrip() for row in s.grid]

    def test_text_lands_where_the_cursor_was_put(self):
        s = screen(20, 6, "\x1b[3;5Hhello")
        self.assertEqual(self.rows(s)[2], "    hello")

    def test_absolute_row_and_column_moves(self):
        # VPA sets the row and keeps the column; CHA sets the column and
        # keeps the row. Getting either of those the other way round is how
        # a chart ends up one row off its axis.
        s = screen(20, 6, "\x1b[4dabc", "\x1b[2Gxy")
        self.assertEqual(self.rows(s)[3], "axy")
        s = screen(20, 6, "\x1b[2;3Hab", "\x1b[5d!")
        self.assertEqual(self.rows(s)[1], "  ab")
        self.assertEqual(self.rows(s)[4], "    !")

    def test_erase_clears_what_it_says_it_clears(self):
        s = screen(10, 3, "\x1b[1;1Habcdefghij", "\x1b[1;4H\x1b[K")
        self.assertEqual(self.rows(s)[0], "abc")

    def test_erase_n_characters_leaves_the_rest(self):
        s = screen(10, 3, "abcdefghij", "\x1b[1;3H\x1b[2X")
        self.assertEqual(self.rows(s)[0], "ab  efghij")

    def test_a_scroll_region_is_where_scrolling_happens(self):
        # DECSTBM 2..4 of six rows: the first and last must not move.
        s = screen(20, 6)
        for i in range(6):
            s.feed(("\x1b[%d;1Hrow%d" % (i + 1, i)).encode())
        s.feed(b"\x1b[2;4r")          # region rows 2-4, and it homes there
        s.feed(b"\x1b[S")             # scroll up one, inside the region
        got = self.rows(s)
        self.assertEqual(got[0], "row0")
        self.assertEqual(got[1], "row2")
        self.assertEqual(got[2], "row3")
        self.assertEqual(got[3], "")
        self.assertEqual(got[5], "row5")

    def test_scroll_down_moves_the_other_way(self):
        s = screen(20, 6)
        for i in range(6):
            s.feed(("\x1b[%d;1Hrow%d" % (i + 1, i)).encode())
        s.feed(b"\x1b[1;6r\x1b[T")
        got = self.rows(s)
        self.assertEqual(got[0], "")
        self.assertEqual(got[1], "row0")
        self.assertEqual(got[5], "row4")

    def test_setting_a_region_homes_the_cursor_to_it(self):
        s = screen(20, 6, "\x1b[3;5r", "here")
        self.assertEqual(self.rows(s)[2], "here")

    def test_a_line_feed_at_the_foot_of_the_region_scrolls_it(self):
        s = screen(20, 4, "\x1b[1;2r", "a\r\nb\r\nc")
        got = self.rows(s)
        self.assertEqual(got[0], "b")
        self.assertEqual(got[1], "c")

    def test_insert_and_delete_line_stay_inside_the_region(self):
        s = screen(20, 6)
        for i in range(6):
            s.feed(("\x1b[%d;1Hrow%d" % (i + 1, i)).encode())
        s.feed(b"\x1b[2;4r\x1b[2;1H\x1b[M")   # delete row1, inside 2..4
        got = self.rows(s)
        self.assertEqual(got[1], "row2")
        self.assertEqual(got[3], "")
        self.assertEqual(got[4], "row4")      # untouched, outside the region

    def test_block_glyphs_survive_a_split_read(self):
        # A multi-byte character cut across two reads must wait, not corrupt.
        s = record.Screen(10, 2)
        s.feed(b"\xe2\x96")
        s.feed(b"\x88x")
        self.assertEqual(self.rows(s)[0], "█x")

    def test_colour_and_dim_are_carried_on_the_cell(self):
        s = screen(20, 2, "\x1b[32mg\x1b[0;2md\x1b[m ")
        self.assertEqual(s.grid[0][0].fg, 2)
        self.assertFalse(s.grid[0][0].dim)
        self.assertTrue(s.grid[0][1].dim)


if __name__ == "__main__":
    unittest.main()

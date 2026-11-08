# Does a change to the page leave every system's page as it was?

`gui/gui.html` is one page for Linux, Windows and macOS, with parts that only one
system shows (`data-only`, `PLATFORM`/`MAC`). Only Linux's is covered by the
Playwright suite in CI; this renders all three the way each system serves them,
so a change meant for one can be seen not to have moved the others.

`shots.py` starts a fake backend (canned state, devices and takes from a real run
in `canned/`, and what Windows' and macOS's `/api/cables` would answer), serves
the page like `Hub::page()` does, and drives headless Firefox (no window opens)
through the main view, the menu, the VBAN and update windows, the channel menus
and, on Windows and macOS, the cables window and the first-start welcome. For
each it writes a screenshot and the visible text.

```sh
# before the change (any copy of the old page), then after:
for p in linux windows macos; do
  python3 shots.py old  /path/to/old/gui.html  $p out/old/$p
  python3 shots.py new  ../../gui.html         $p out/new/$p attr   # `attr`: data-platform on <html>, as the server does
done
python3 compare.py out        # "ALL IDENTICAL", or what differs, per system and scenario
```

Needs Firefox and `geckodriver` on the PATH (the snap's `/snap/bin/geckodriver` is
tried as well). Rendering is deterministic, so identical pixels mean identical
pages; a difference you meant to make shows up as exactly that.

Used for step 4 of the GUI-layer work (one `gui.html`): the merged page matched the
three old pages in all 26 scenarios.

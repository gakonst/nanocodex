## Music Computer Use

### Searching

In order to search for music, click the Search row in the sidebar and then use `set-value` on the search text field. The search will be submitted automatically; if results don't appear, run `get-state` again and check whether the results have loaded.

If the search field is not visible, make sure the sidebar is scrolled all the way to the top. Note that the `filterField` (a search field shown when clicking `filterBtn`) is for filtering what's in the current view, not for searching the entire library or Apple Music catalog.

To find a playlist, either perform a search (make sure 'Your Library' is selected in the search results), or scroll down in the sidebar.

### Navigation

In order to scroll an element, use its "Scroll Up" or "Scroll Down" actions. To scroll faster, use parallel function calling to scroll by multiple at once.

Note: after selecting an item in the sidebar, note that you may be unexpectedly drilled into a sub-view. In order to go back to the root level, use the `backBtn`.

### Playback

To play a track, double-click it.

To add to the playback queue ("Playing Next"), use the "More" button and then press "Play Next" or "Play Last". If the "More" button is unavailable, try a right-click.

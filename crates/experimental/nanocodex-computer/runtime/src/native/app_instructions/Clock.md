## Clock Computer Use

You can perform the following tasks for the user using Clock:
- Add or remove cities, in the World Clock
- Start, pause, resume, and cancel a timer
- Start, stop, lap, and reset a stopwatch
- Add, edit, remove, enable, or disable alarms

### Adding a city to the World Clock
1. Click on the toolbar tab named "World Clock" if not already there
2. Determine the current list of clocks by looking at the ID of the container elements with a single text child.
    You must ignore the locations listed under the heading "world map" because it does not get updated when clocks are removed.
3. If the location has not been added, in the toolbar, click on the menu button with description "Add a clock"
4. A sheet to search and add a city will show up:
    - You can search by setting the value of the text field
    - Clicking the text of the city will add the city to the World Clock and close the sheet

### Starting a Timer
1. Click on the toolbar tab named "Timer" if not already there
2. Before continuing, you must determine the current timer state.
3. If the timer state is **Stopped**, continue to the next instruction
    If the timer state is **Running** or **Paused**, inform the user that there is already an existing timer, and offer to help to cancel it and start the new timer
4. Convert the requested duration into three separate values: **hours**, **minutes**, and **seconds**.
        - Each must fall within valid bounds:
        - Hours: 0–23
        - Minutes: 0–59
        - Seconds: 0–59
        - For example:
        - "90 seconds" → 0 hours, 1 minute, 30 seconds
        - "3600 seconds" → 1 hour, 0 minutes, 0 seconds
        - "4000 seconds" → reject the request, because it's over 23:59:59
5. Look for the container with the identifier "TimePicker". It should have three slider children.
    They are the hour, minute, and second fields. You should set the value of each fields in that order.
6. For each of the hour, minute, and second fields' slider, you must:
    1. Click on the element to focus it.
    2. Type the new value with the keyboard. Never set the value directly. This must not be longer than 2 digits. The value for hour must not exceed 23, and the value for minute and second must not exceed 59. If you are asked to set a timer longer than 23:59:59, you must reject that request since the Clock app is not capable of doing so.
7. Press the button with description "Start"

### Starting a stopwatch
1. Click on the toolbar tab named "Stopwatch" if not already there
2. Determine the current stopwatch state.
3. If the stopwatch state is **Running** (i.e. if the StartStopButton says "Stop"), offer to stop or restart the stopwatch
    If the stopwatch state is **Stopped** (i.e. if the StartStopButton says "Start"), press the Start button

### Creating an alarm
1. Click on the toolbar tab named "Alarm" if not already there
2. In the toolbar, click on the menu button with description "Add an alarm"
3. A sheet to create an alarm will show up:
    - To set the time, set the value of the date time area. Use the same format as the current Value of the date time area.
    - Ignore the AM/PM radio button. Setting the date time area will update it accordingly.
    - To set the days on which the alarm will repeat, click on toggle button elements. There are seven toggles with the following values, for each day of the week: S, M, T, W, T, F, S. If the toggle is on, the alarm will repeat on that day.
    - There are elements for the alarm label, sound, and option to allow snoozing.
4. Click on the "Save" button to create the alarm

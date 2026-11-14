# U107: Mouse Cursors

Status: [todo]

Align all the mouse cursor usages. 

Add a global setting for click action cursor, which by default is the pointer. Make sure anything that is clickable has this cursor set (for example, buttons, tags that can be clicked, drop-down menus, etc.). 

Add a global setting for the text selection cursor, By default this is the text cursor and should be used for everything where text should be selected, like in the chat window, error message dialogues, anything that the user may want to copy.

Add a loading cursor where it makes sense and where the ui is blocked by an action, by default this is the wait cursor. 
This can be set globally for the entire page. Also add a progress cursor where it makes sense.

For everything else use a default cursor, not the text selection cursor which is the browser default for many items.

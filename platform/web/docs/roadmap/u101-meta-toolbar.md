# U101: Meta Toolbar

Status: [in progress]

In the listing where all the icons are, refactor this to an official component called meta Toolbar Which also can be used in other places .

For each item added to the meta toolbar, allow:
  
- an icon
- a meta value
- a status color
- text
- a tooltip
- a priority
- a preferred alignment (start/end)
- show in compact: true/false if it should be shown in compact mode (like in listing)
- show in full: true/false if it should be shown in full mode (like in chat header)
- click handler (to support the same functionality with resource tags like environment, etc)

Then we use the priority and preferred alignment to build the actual visual representation of the toolbar, Depending on the space and size .

This new meta toolbar is then used on the listing for sessions and bots as well as the actual session page: at the top next to the title (compact mode), and also in a larger format just below where we now have the additional meta information and tags, etc. 

In compact mode we just use an icon plus a meta value, like a number. In the full mode we use something similar to the resources we use: actual tags.

First implement the component and add some strong use-case examples in the debug/ui-overview page. Notifyme when done - I will review it.

Then refactor the bots and sessions to use this new common meta toolbar so that each session and each bot provides a single one, which is then displayed in multiple ways with different settings. There is a single interface for the chat windows/entities to provide the meta tools and we can always display them using the different compact and full stylings where we need them.

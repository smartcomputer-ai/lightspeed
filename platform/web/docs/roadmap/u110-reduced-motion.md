# U110: Reduced Motion

Status: [todo]

Go through the entire UI and double-check that when a reduced motion flag is set, it is properly respected. For example animations and transitions should be turned off and especially the starscape should be static and not animated. 

Make sure that the reduced motion goes through a common library call to test for it, which can also be integrated in the user's profile as an option. By default it's on. If the browser has reduced motion off, then turn it off. 

But all the settings come from the user's profile setting not the browser. The browser just sets the default correctly.

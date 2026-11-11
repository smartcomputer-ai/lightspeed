# U104: Onboarding

Status: [todo]

Let's completely revamp the onboarding system with a new welcome dialog that can be built out later for more onboarding tools and settings (like asking the user which theme, what kind of preferences, setting up some defaults, etc.). 

For the moment we only want a nice onboarding screen. It should be a dialog that starts when the platform is first loaded, with an option to never show it again. When the user selects "Don't show again," it is not shown on the next load. 

In demo mode there is a small warning message in the warning colors, added to the welcome screen/text, that says "Demo Mode" as the title. Then this demo runs completely in your browser on scripted data. No real LLMs are used. Nothing is saved or sent anywhere. Reset the demo data at any time from the settings."

The onboarding welcome dialog should have a landscape-style starscape with the Lightspeed logo over it and also the lightning bolt and the hand. You can see the ls.bot website and how that works (D:\Code\Lightspeed\ls-site).  with the starscape at the center. Below it should say "Welcome to Lightspeed." Below that there is a short introduction text. For now use lorem ipsum (circa 3 senteces). The dialog has an OK button and, on the left side, a small checkbox to say "Don't show anymore."  

Note that the current implementation for the demo mode warning has some sort of injected HTML into the page that briefly flashes until the CSS is shown. Absolutely make sure that this no longer exists and is replaced strictly with a UI that is invoked through the frontend and doesn't have some sort of hardcoded HTML on the page.

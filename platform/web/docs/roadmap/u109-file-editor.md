# U109: File Editor

Status: [todo]

Create a new component called File Editor. It should support different file formats but foremost right now we want to support source code files. Use the following technology for the formatting and editing. 

https://microsoft.github.io/monaco-editor/

The component should later also support image, basic editing, and other formats that are common in corporate environments but those will be added later. But the component needs to be prepared to have different editing providers for the different file types. 

For the source code also create a new component called Source Code Editor, Which is integrated into the file editor component .

The file editor also has a preview mode, which then can render some files as a preview, for example, Markdown files or HTML files. 

Make sure the Monaco integration is very efficient, using tree shaking, etc., for only the features we need. Also make sure to set it up so that it uses the correct darker light theme. I also want lazy loading of the languages needed for the files being edited.

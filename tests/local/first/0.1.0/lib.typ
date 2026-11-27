// Outdated import
#import "@local/second:0.1.0"

#import "src/example.typ": add

// Will fail, because the HTML target isn't specified.
#html.p[Hello there.]

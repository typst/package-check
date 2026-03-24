# Integration tests

`run` is a bash script that can check the output of the tool against a
reference.

The reference outputs are in `ref/`. To add a package to test, create an empty
file in this directory, named `NAMESPACE+NAME+VERSION.json`. If the namespace
is `preview`, the tool will fetch the package from Universe. It can also be
`local`, in which case the package source will be taken from the directory of
the same name.

To update a reference output (or define it for the first time when adding a
package), simply copy it from the actual output in the `out` directory.

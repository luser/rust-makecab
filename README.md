[![Build Status](https://travis-ci.org/luser/rust-makecab.svg?branch=master)](https://travis-ci.org/luser/rust-makecab) [![Build status](https://ci.appveyor.com/api/projects/status/qe0ou1wihcwuul78/branch/master?svg=true)](https://ci.appveyor.com/project/luser/rust-makecab/branch/master)

This crate implements a subset of the [Microsoft cabinet format](https://msdn.microsoft.com/en-us/library/bb417343.aspx#cabinet_format), allowing the creation of cabinet files containing a single file compressed with [MSZIP](https://msdn.microsoft.com/en-us/library/bb417343.aspx#microsoftmszipdatacompressionformat) compression.

A `make_cab` library function is provided, as well as a `makecab` binary that supports commandline options equivalent to Microsoft's implementation (but only a subset of them).

```
Any copyright is dedicated to the Public Domain.
http://creativecommons.org/publicdomain/zero/1.0/
```

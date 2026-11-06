# CMake's own default is the shared runtime, and it is applied after the flags
# the Rust build passes, so C and C++ codecs would still need msvcp140.dll.
# Rust links the release runtime in every profile, so this does too.
set(CMAKE_MSVC_RUNTIME_LIBRARY MultiThreaded)

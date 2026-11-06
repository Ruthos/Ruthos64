# CMake's own default is the shared runtime, and it is applied after the flags
# the Rust build passes, so C and C++ codecs would still need msvcp140.dll.
# Rust links the release runtime in every profile, so this does too.
set(CMAKE_MSVC_RUNTIME_LIBRARY MultiThreaded)

# The cmake crate replaces the Release flags on MSVC and takes the optimisation
# out, so without this the codecs in a release build are not optimised at all.
foreach(lang C CXX)
    if(DEFINED CMAKE_${lang}_FLAGS_RELEASE AND NOT CMAKE_${lang}_FLAGS_RELEASE MATCHES "/O2")
        set(CMAKE_${lang}_FLAGS_RELEASE "${CMAKE_${lang}_FLAGS_RELEASE} /O2 /Ob2 /DNDEBUG" CACHE STRING "" FORCE)
    endif()
endforeach()

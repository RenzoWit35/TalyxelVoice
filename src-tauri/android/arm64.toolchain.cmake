# What the C/C++ dependencies (whisper.cpp, aws-lc) build with for Android.
# The cmake crate only passes a toolchain file, not ANDROID_ABI, and the NDK's
# file defaults to 32-bit ARM without it. Set via CMAKE_TOOLCHAIN_FILE_aarch64_linux_android.
set(ANDROID_ABI arm64-v8a)
set(ANDROID_PLATFORM 26)
include($ENV{NDK_HOME}/build/cmake/android.toolchain.cmake)

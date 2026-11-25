cmake_minimum_required(VERSION 3.16)
project(vanta_native LANGUAGES C CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_POSITION_INDEPENDENT_CODE ON)

# Release/optimized native codegen flags: enable full -O3 vectorization,
# devirtualization/inlining without PLT semantic-interposition barriers, and
# section GC friendliness across vanta_jni_shim, vanta_liblog, jnivm, and linker.
add_compile_options(
  $<$<NOT:$<CONFIG:Debug>>:-O3>
  $<$<NOT:$<CONFIG:Debug>>:-fno-semantic-interposition>
  $<$<NOT:$<CONFIG:Debug>>:-ffunction-sections>
  $<$<NOT:$<CONFIG:Debug>>:-fdata-sections>
  $<$<AND:$<NOT:$<CONFIG:Debug>>,$<COMPILE_LANGUAGE:CXX>>:-fvisibility-inlines-hidden>
)

# Clang is not a preference. AOSP bionic uses C11 _Atomic inside C++ headers
# (bionic_elf_tls.h, bionic_fdsan.h) — a Clang extension GCC rejects, producing
# 144 errors on an otherwise clean tree. See docs/base-evaluation.md §2.1.
if(NOT CMAKE_CXX_COMPILER_ID MATCHES "Clang")
  message(FATAL_ERROR
    "The native subtree requires Clang (found ${CMAKE_CXX_COMPILER_ID}). "
    "AOSP bionic does not build with GCC.")
endif()

# The AOSP bionic linker, via minecraft-linux's host wrapper. Target: `linker`.
# crates/vanta-linker-sys/build.rs passes a patched overlay of the
# submodule here (patches/apply.rs); the submodule itself is the default for
# a configure run by hand.
if(NOT VANTA_LINKER_DIR)
  set(VANTA_LINKER_DIR ${CMAKE_CURRENT_SOURCE_DIR}/../third_party/mcpelauncher-linker)
endif()
add_subdirectory(${VANTA_LINKER_DIR}
                 ${CMAKE_CURRENT_BINARY_DIR}/mcpelauncher-linker)

# Undo the upstream `-DPATH_MAX=256`, which is a stack overflow waiting for a
# long enough path.
#
# third_party/mcpelauncher-linker/CMakeLists.txt does:
#
#     target_compile_definitions(linker PRIVATE PATH_MAX=256 _GNU_SOURCE)
#
# so every `char buf[PATH_MAX]` in the linker port is 256 bytes — seven of them,
# in linker.cpp, linker_utils.cpp, linker_main.cpp and linker_config.cpp — while
# the `realpath`, `readlink` and `snprintf` those buffers are handed to are the
# *host's*, which are entitled to write up to the real PATH_MAX of 4096.
#
# glibc caught it. Inside the Flatpak, whose runtime fortifies these calls, the
# client aborted at linker init with `*** buffer overflow detected ***`, and the
# check reads exactly this:
#
#     lea    -0x130(%rbp),%rsi
#     mov    $0x100,%edx          ; 256, the buffer this define created
#     call   __realpath_chk@plt   ; refuses anything under PATH_MAX
#
# The host build makes the same calls through plain `realpath@plt` with no
# `__realpath_chk` anywhere in the binary, so it has carried the same undersized
# buffers unchecked for as long as this has existed. **The host build working is
# the less trustworthy of the two results**, not the baseline the Flatpak
# departed from: a resolved path over 256 bytes smashes that stack frame there
# today and nothing reports it.
#
# Corrected to 4096 rather than deleted, and the difference was measured. The
# first attempt removed the define outright on the theory that `<limits.h>`
# would then supply the real value; the build failed immediately with
#
#     linker_utils.cpp:216:22: error: use of undeclared identifier 'PATH_MAX'
#
# because these translation units are compiled with `-include compat.h` against
# `include/`, and never see a `<limits.h>` that defines it. Supplying the macro
# is exactly why upstream's line exists. Only its value was wrong.
#
# 4096 is what the kernel uapi header in this same submodule says
# (bionic/libc/kernel/uapi/linux/limits.h), and what both this host and the
# Flatpak SDK report from `<limits.h>` — checked, not assumed.
#
# Kept on our side rather than patched into the submodule because the Flatpak
# manifest pins that repository by commit and clones it fresh: a local edit
# there would fix `cargo build` and leave the package broken, which is the
# worse half to get right by accident.
get_target_property(_linker_defs linker COMPILE_DEFINITIONS)
list(REMOVE_ITEM _linker_defs "PATH_MAX=256")
list(APPEND _linker_defs "PATH_MAX=4096")
set_target_properties(linker PROPERTIES COMPILE_DEFINITIONS "${_linker_defs}")

# PipeWire, for opensles.cpp's audio backend. Headers only, and only if
# present: libpipewire-0.3.so is never linked (dlopen'd at run time instead,
# see pipewire_backend.cpp), so this only decides whether that file compiles
# its real implementation or the honest "no audio" fallback it falls back to
# when pipewire-devel is absent. A tree with neither still builds.
find_package(PkgConfig QUIET)
if(PkgConfig_FOUND)
  pkg_check_modules(PIPEWIRE QUIET libpipewire-0.3)
  pkg_check_modules(SPA QUIET libspa-0.2)
endif()

# pkg-config saying "not found" is not the same as the headers being absent,
# and believing it cost this project every sample of audio it has ever tried to
# play. On the machine this was found on, `pkg-config` first on PATH is
# Homebrew's pkgconf, whose compiled-in `pc_path` is its own three Cellar
# directories and does not include `/usr/lib64/pkgconfig` — so
# `pkg_check_modules` reported libpipewire-0.3 missing on a host with
# pipewire-devel 1.6.8 installed, headers, daemon and all. The build then did
# exactly what it was told to: compiled the `#else` branch of
# pipewire_backend.cpp, and `slCreateEngine` reported
# SL_RESULT_FEATURE_UNSUPPORTED for reasons that had nothing to do with audio.
# Nothing announced this; the status message below is the only place it was
# ever visible, and it scrolls past inside a cargo build.
#
# So do not take pkg-config's word for it. Only headers are wanted here — the
# library is dlopen'd, never linked (see pipewire_backend.cpp) — and headers
# are something CMake can look for directly. `find_path` searches the platform
# include directories no matter what any one pkg-config binary believes its
# search path to be, which makes this fallback strictly more reliable than the
# thing it backs up rather than merely a second guess at it.
if(NOT PIPEWIRE_FOUND)
  find_path(PIPEWIRE_FALLBACK_INCLUDE_DIR pipewire/pipewire.h
            PATH_SUFFIXES pipewire-0.3)
  find_path(SPA_FALLBACK_INCLUDE_DIR spa/param/audio/format-utils.h
            PATH_SUFFIXES spa-0.2)
  if(PIPEWIRE_FALLBACK_INCLUDE_DIR AND SPA_FALLBACK_INCLUDE_DIR)
    set(PIPEWIRE_FOUND TRUE)
    set(PIPEWIRE_INCLUDE_DIRS ${PIPEWIRE_FALLBACK_INCLUDE_DIR} ${SPA_FALLBACK_INCLUDE_DIR})
    set(PIPEWIRE_VERSION "(version unknown: found by header search, not pkg-config)")
    message(STATUS "OpenSL ES audio: pkg-config did not find libpipewire-0.3, but its "
                    "headers are present at ${PIPEWIRE_FALLBACK_INCLUDE_DIR} and "
                    "${SPA_FALLBACK_INCLUDE_DIR}. Using those. If this is a surprise, the "
                    "usual cause is a pkg-config earlier on PATH whose pc_path excludes the "
                    "system one -- check `pkg-config --variable pc_path pkg-config`.")
  endif()
endif()

# PulseAudio, the second host backend (ADR-023). Same arrangement as PipeWire
# above and for the same reasons: headers only, optional, and `libpulse.so.0`
# is dlopen'd at run time rather than linked, so a machine without it degrades
# to "that backend is unavailable" instead of to a client that will not start.
#
# **`pkg_check_modules` is not trusted for this one, and the failure mode is
# worse than PipeWire's was.** On the machine this was written on, the
# `pkg-config` first on PATH is Homebrew's, and it *succeeds* for `libpulse` --
# resolving to headers under its own Cellar for a library that is not the system
# one. PipeWire at least failed loudly and could be rescued by `find_path`; this
# would have compiled against the wrong headers and said nothing. So the header
# search is the primary here and pkg-config is not consulted at all.
find_path(PULSE_INCLUDE_DIR pulse/pulseaudio.h)
if(PULSE_INCLUDE_DIR AND NOT PULSE_INCLUDE_DIR MATCHES "linuxbrew|Cellar|homebrew")
  set(PULSE_FOUND TRUE)
  message(STATUS "PulseAudio backend: headers at ${PULSE_INCLUDE_DIR}; libpulse.so.0 will be "
                  "dlopen'd at run time. Selected with VANTA_AUDIO_HOST=pulse.")
elseif(PULSE_INCLUDE_DIR)
  message(STATUS "PulseAudio backend: ignoring headers at ${PULSE_INCLUDE_DIR} because they are "
                  "not the system ones -- a Homebrew prefix here compiles against a different "
                  "libpulse from the one that will be loaded. Install pulseaudio-libs-devel.")
  set(PULSE_FOUND FALSE)
else()
  message(STATUS "PulseAudio backend: no pulse/pulseaudio.h; building the honest "
                  "\"unavailable\" arm. Install pulseaudio-libs-devel for it.")
  set(PULSE_FOUND FALSE)
endif()

# spa-0.2 ships its own .pc and lives in its own include directory; when
# pkg-config *did* answer for libpipewire-0.3 it will not have mentioned SPA,
# and every format-negotiation header this backend includes is SPA's.
if(PIPEWIRE_FOUND AND SPA_INCLUDE_DIRS)
  list(APPEND PIPEWIRE_INCLUDE_DIRS ${SPA_INCLUDE_DIRS})
endif()

if(PIPEWIRE_FOUND)
  message(STATUS "OpenSL ES audio: pipewire-devel ${PIPEWIRE_VERSION} found; "
                  "libpipewire-0.3 will be dlopen'd at run time, not linked.")
else()
  message(STATUS "OpenSL ES audio: pipewire-devel not found at configure time "
                  "(pkg-config could not find libpipewire-0.3); slCreateEngine "
                  "will report SL_RESULT_FEATURE_UNSUPPORTED and Roblox will run "
                  "without audio. Install pipewire-devel and reconfigure to "
                  "enable it.")
endif()

# Thin extern "C" surface over the linker's C++ API, so Rust can drive it.
# Android liblog. Roblox narrates its own startup through it, so this is the
# runtime layer's most valuable diagnostic. It is not the source of
# onLogLine/onJoin: neither event exists, see the header of liblog.cpp.
#
# `aaudio.cpp` is here rather than beside the JNI shim because it belongs with
# `pipewire_backend.cpp`, and it needs no PipeWire headers of its own — its two
# local headers deliberately carry no PipeWire types. So the AAudio bridge
# compiles identically on a tree with no
# pipewire-devel, and honestly refuses every stream there, exactly as
# `slCreateEngine` does.
add_library(vanta_liblog STATIC
  liblog.cpp system_paths.cpp netdb_compat.cpp foreign_alloc.cpp opensles.cpp pipewire_backend.cpp
  pulse_backend.cpp alsa_backend.cpp oss_backend.cpp aaudio.cpp
  aaudio_input_callback.cpp thread_trace.cpp)
# Compiled unconditionally, like every other file here: without the headers it
# is its own honest "unavailable" arm, so the selector gets a truthful answer
# rather than a link error. Only the define is conditional.
if(PULSE_FOUND)
  target_include_directories(vanta_liblog PRIVATE ${PULSE_INCLUDE_DIR})
  target_compile_definitions(vanta_liblog PRIVATE VANTA_HAVE_PULSE=1)
endif()
# ALSA, the third backend. Same rule as the two above: headers optional,
# `libasound.so.2` dlopen'd and never linked, and the same refusal to trust
# pkg-config for it -- Homebrew's pkgconf answers for `alsa` with its own Cellar
# headers on this machine, which is a silent wrong answer rather than a loud
# missing one.
find_path(ALSA_INCLUDE_DIR alsa/asoundlib.h)
if(ALSA_INCLUDE_DIR AND NOT ALSA_INCLUDE_DIR MATCHES "linuxbrew|Cellar|homebrew")
  target_include_directories(vanta_liblog PRIVATE ${ALSA_INCLUDE_DIR})
  target_compile_definitions(vanta_liblog PRIVATE VANTA_HAVE_ALSA=1)
  message(STATUS "ALSA backend: headers at ${ALSA_INCLUDE_DIR}; libasound.so.2 will be "
                  "dlopen'd at run time. Selected with VANTA_AUDIO_HOST=alsa.")
else()
  message(STATUS "ALSA backend: no usable alsa/asoundlib.h; building the honest "
                  "\"unavailable\" arm. Install alsa-lib-devel for it.")
endif()

# OSS, the fourth. Simpler than all three above -- no client library to find, no
# server to talk to, just <sys/soundcard.h> and /dev/dsp -- so the only question
# is whether the header exists. Nothing is linked and nothing is dlopen'd.
#
# ADR-023 deferred this on the grounds that Linux has not used OSS since ALSA
# replaced it and the remaining users are on FreeBSD. Somebody asked for it, so
# "nobody needs this" turned out to be a claim about who had spoken up. It is
# opt-in behind VANTA_AUDIO_HOST=oss and is never selected for anybody who did
# not ask -- which matters more here than for the others, because /dev/dsp is
# exclusive on most drivers.
#
# This block must stay *after* `add_library(vanta_liblog ...)`. Put before it
# once, and `target_include_directories` fails on a target that does not exist
# yet -- which reads as a problem with OSS rather than with where the lines sit.
find_path(OSS_INCLUDE_DIR sys/soundcard.h)
if(OSS_INCLUDE_DIR AND NOT OSS_INCLUDE_DIR MATCHES "linuxbrew|Cellar|homebrew")
  target_include_directories(vanta_liblog PRIVATE ${OSS_INCLUDE_DIR})
  target_compile_definitions(vanta_liblog PRIVATE VANTA_HAVE_OSS=1)
  message(STATUS "OSS backend: sys/soundcard.h at ${OSS_INCLUDE_DIR}. Selected with "
                  "VANTA_AUDIO_HOST=oss. /dev/dsp is exclusive on most drivers, so "
                  "nothing else on the machine gets sound while it is held.")
else()
  message(STATUS "OSS backend: no sys/soundcard.h; building the honest "
                  "\"unavailable\" arm.")
endif()

if(PIPEWIRE_FOUND)
  target_include_directories(vanta_liblog PRIVATE ${PIPEWIRE_INCLUDE_DIRS})
  target_compile_definitions(vanta_liblog PRIVATE VANTA_HAVE_PIPEWIRE=1)

  # Checks pipewire_backend.cpp's buffer-queue fill logic (the underrun ->
  # silence rule in particular) without a live PipeWire session, a daemon, or
  # any way to produce sound — see pipewire_backend_test.cpp for why that
  # property matters here specifically. Run as part of the ordinary build
  # rather than left for someone to remember to invoke separately: a check
  # that only runs when asked for is a check that silently stops running.
  # `pulse_backend.cpp` comes along because `pipewire_backend.cpp`'s factory
  # now calls into it. Deliberately built here *without* `VANTA_HAVE_PULSE`,
  # so the test exercises the arm that reports the backend unavailable -- which
  # is the arm every machine without pulseaudio-libs-devel will run, and the one
  # a fallback bug would hide in.
  add_executable(vanta_pipewire_backend_test EXCLUDE_FROM_ALL
    pipewire_backend.cpp pulse_backend.cpp alsa_backend.cpp oss_backend.cpp
    pipewire_backend_test.cpp)
  target_include_directories(vanta_pipewire_backend_test PRIVATE ${PIPEWIRE_INCLUDE_DIRS})
  target_compile_definitions(vanta_pipewire_backend_test PRIVATE VANTA_HAVE_PIPEWIRE=1)
  # Without this the asserts are compiled out of a release build, which is what
  # this binary was for a long time: it printed "ok" for checks that never ran,
  # and one of them was already wrong.
  target_compile_options(vanta_pipewire_backend_test PRIVATE -UNDEBUG)
  if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
    add_custom_command(TARGET vanta_pipewire_backend_test POST_BUILD
      COMMAND vanta_pipewire_backend_test
      COMMENT "Running pipewire_backend fill_pcm checks (no PipeWire session needed)")
  endif()
  add_dependencies(vanta_liblog vanta_pipewire_backend_test)
endif()

# The recorder's copy of the engine's sound (record_tap.h, ADR-059). Pure ring
# and mixing logic, so it runs at build time with no sound server, like the
# checks around it.
add_executable(vanta_record_tap_test EXCLUDE_FROM_ALL record_tap_test.cpp)
target_compile_options(vanta_record_tap_test PRIVATE -UNDEBUG)
if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
  add_custom_command(TARGET vanta_record_tap_test POST_BUILD
    COMMAND vanta_record_tap_test
    COMMENT "Running recorder audio tap checks")
endif()
add_dependencies(vanta_liblog vanta_record_tap_test)

add_executable(vanta_aaudio_input_callback_test EXCLUDE_FROM_ALL
  aaudio_input_callback.cpp aaudio_input_callback_test.cpp)
target_compile_options(vanta_aaudio_input_callback_test PRIVATE -UNDEBUG)
if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
  add_custom_command(TARGET vanta_aaudio_input_callback_test POST_BUILD
    COMMAND vanta_aaudio_input_callback_test
    COMMENT "Running AAudio input callback lifecycle checks")
endif()
add_dependencies(vanta_liblog vanta_aaudio_input_callback_test)

add_executable(vanta_aaudio_input_test EXCLUDE_FROM_ALL
  aaudio.cpp aaudio_input_callback.cpp aaudio_input_test.cpp)
target_compile_options(vanta_aaudio_input_test PRIVATE -UNDEBUG)
if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
  add_custom_command(TARGET vanta_aaudio_input_test POST_BUILD
    COMMAND vanta_aaudio_input_test
    COMMENT "Running AAudio callback and blocking input ABI checks")
endif()
add_dependencies(vanta_liblog vanta_aaudio_input_test)

# The two libc allocators refused in foreign_alloc.cpp. Run at build time, like
# the tests around it, so a shim that stops refusing cannot ship unnoticed.
add_executable(vanta_foreign_alloc_test EXCLUDE_FROM_ALL
  foreign_alloc.cpp foreign_alloc_test.cpp)
target_compile_options(vanta_foreign_alloc_test PRIVATE -UNDEBUG)
if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
  add_custom_command(TARGET vanta_foreign_alloc_test POST_BUILD
    COMMAND vanta_foreign_alloc_test
    COMMENT "Running foreign-allocation shim checks")
endif()
add_dependencies(vanta_liblog vanta_foreign_alloc_test)

add_library(vanta_linker_shim STATIC shim.cpp)
target_link_libraries(vanta_linker_shim PUBLIC linker)

install(TARGETS vanta_linker_shim vanta_liblog linker ARCHIVE DESTINATION lib)

# Vendored x86-64 CPU feature emulator (see third_party/libbadcpu/README.md).
# Independent of the bionic subtree; built here so there is one native build.
#
# **Only on x86-64, and this is a build break rather than a nicety.**
# `cpuid.cpp` includes `<cpuid.h>` and calls `__get_cpuid`, neither of which
# exists on aarch64, so an ARM build fails at compile time here -- the first
# thing that breaks, before anything interesting has been attempted.
# `docs/multiarch.md` already says an ARM64 host "needs no feature emulator",
# and it also claims this is gated in the build; it was not, and the claim was
# written against a Meson build this project does not use.
#
# Nothing links it today: `crates/vanta-linker-sys/build.rs` names six
# archives and `badcpu` is not among them. So gating it costs exactly nothing
# and removes the first obstacle on a port nobody has started.
if(CMAKE_SYSTEM_PROCESSOR MATCHES "^(x86_64|amd64|AMD64)$")
  add_library(badcpu STATIC
    ../third_party/libbadcpu/src/badcpu.cpp
    ../third_party/libbadcpu/src/cpuid.cpp
    ../third_party/libbadcpu/src/decoder.cpp
    ../third_party/libbadcpu/src/emulator.cpp
    ../third_party/libbadcpu/src/signal_handler.cpp
  )
  target_include_directories(badcpu PUBLIC ../third_party/libbadcpu/include)
  install(TARGETS badcpu ARCHIVE DESTINATION lib)
endif()

# JNI virtual machine. Roblox's native code calls into Java through 518 statically
# registered natives and expects a JavaVM to call back into; libjnivm supplies both
# sides without a JVM. Debug mode (JNI_DEBUG) enables VM::GenerateClassDump, but
# also forces every InternalFindClass / NewGlobalRef / NewWeakGlobalRef off the
# vm->classes hash map onto an O(N) heap-allocating namespace walk. Keep it OFF
# by default for fast gameplay and opt in via -DVANTA_JNI_DEBUG=ON when dumping.
option(VANTA_JNI_DEBUG "Enable libjnivm class-dump tracking (slower FindClass)" OFF)
set(JNIVM_ENABLE_DEBUG ${VANTA_JNI_DEBUG} CACHE BOOL "" FORCE)

# JNIVM_ENABLE_TRACE printfs *every* JNI call libjnivm handles. That is
# genuinely how most of the framework surface was discovered — `String.getBytes`
# being unresolved on the text-entry path was found in exactly this output, and
# nothing else would have shown it.
#
# It is also ruinous to leave on. The engine polls `MotionEvent.getRawX`,
# `getRawY` and `getAxisValue` per pointer per frame, so this writes several
# thousand unbuffered lines a second, forever, on the hot path. It was on
# unconditionally and it made the whole machine lag, not just Vanta.
#
# So: off by default, and one flag to get it back when diagnosing.
#
#     cmake -DVANTA_JNI_TRACE=ON ...
#
# `--dump-classes` remains available without it (that is JNIVM_ENABLE_DEBUG,
# above, which costs nothing per call) and answers "what did Roblox reach for"
# for most purposes. Reach for the trace when the question is "and what happened
# when it did".
option(VANTA_JNI_TRACE "Log every JNI call libjnivm handles (very slow)" OFF)
set(JNIVM_ENABLE_TRACE ${VANTA_JNI_TRACE} CACHE BOOL "" FORCE)
set(JNIVM_ENABLE_TESTS OFF CACHE BOOL "" FORCE)
set(JNIVM_BUILD_EXAMPLES OFF CACHE BOOL "" FORCE)

# libjnivm's own seam, taken rather than patched. Its CMakeLists says
#
#     if(TARGET logger)
#         target_compile_definitions(jnivm PRIVATE HAVE_LOGGER)
#         target_link_libraries(jnivm PRIVATE logger)
#     endif()
#
# so defining a target with that name before the `add_subdirectory` below is the
# entire integration. Nothing in `third_party/` changes, which matters because
# the JNI surface is the part of this tree most likely to want a newer libjnivm
# and a vendored dependency carrying local edits is one nobody updates.
#
# What it captures is `Constructed Unresolved symbol` — the only notice Vanta
# gets that the engine asked for a Java class, method or field nobody has
# written. See `jnivm_logger/log.cpp`.
add_library(logger STATIC jnivm_logger/log.cpp)
target_include_directories(logger PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/jnivm_logger)
set_target_properties(logger PROPERTIES POSITION_INDEPENDENT_CODE ON)

add_subdirectory(${CMAKE_CURRENT_SOURCE_DIR}/../third_party/libjnivm
                 ${CMAKE_CURRENT_BINARY_DIR}/libjnivm EXCLUDE_FROM_ALL)
# libjnivm's vm.h uses std::unordered_map<std::string, ...> without including
# <string>; when JNI_DEBUG is OFF, namespace.h no longer pulls <string> in.
target_compile_options(jnivm PUBLIC $<$<COMPILE_LANGUAGE:CXX>:-include> $<$<COMPILE_LANGUAGE:CXX>:string>)

# audio_classes.cpp needs no PipeWire headers and so is compiled
# unconditionally: pipewire_backend.h deliberately carries no PipeWire types
# (see its own header comment), so the Java audio surface builds identically
# either way and reports an honestly empty device list on a tree that has no
# backend behind it.
add_library(vanta_jni_shim STATIC jni_shim.cpp android_classes.cpp game_activity.cpp init_params.cpp accessibility.cpp cookies.cpp audio_classes.cpp deeplink.cpp clipboard.cpp permissions_transport.cpp local_storage.cpp legacy_stdio.cpp platform_classes.cpp battery.cpp unanswered_classes.cpp)
target_link_libraries(vanta_jni_shim PUBLIC jnivm)

add_executable(vanta_jni_shim_test EXCLUDE_FROM_ALL jni_shim.cpp jni_shim_test.cpp)
target_compile_options(vanta_jni_shim_test PRIVATE -UNDEBUG)
target_link_libraries(vanta_jni_shim_test PRIVATE jnivm)
if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
  add_custom_command(TARGET vanta_jni_shim_test POST_BUILD
    COMMAND vanta_jni_shim_test
    COMMENT "Running JNI worker attachment checks")
endif()
add_dependencies(vanta_jni_shim vanta_jni_shim_test)

add_executable(vanta_netdb_compat_test EXCLUDE_FROM_ALL netdb_compat.cpp netdb_compat_test.cpp)
target_compile_options(vanta_netdb_compat_test PRIVATE -UNDEBUG)
target_link_libraries(vanta_netdb_compat_test PRIVATE pthread)
if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
  add_custom_command(TARGET vanta_netdb_compat_test POST_BUILD
    COMMAND vanta_netdb_compat_test
    COMMENT "Running netdb_compat DNS cache checks")
endif()
add_dependencies(vanta_jni_shim vanta_netdb_compat_test)

add_executable(vanta_permissions_transport_test EXCLUDE_FROM_ALL
  permissions_transport.cpp permissions_transport_test.cpp)
target_compile_options(vanta_permissions_transport_test PRIVATE -UNDEBUG)
target_include_directories(vanta_permissions_transport_test PRIVATE
  ${CMAKE_CURRENT_SOURCE_DIR}/../third_party/libjnivm/include)
if(NOT CMAKE_CROSSCOMPILING OR CMAKE_CROSSCOMPILING_EMULATOR)
  add_custom_command(TARGET vanta_permissions_transport_test POST_BUILD
    COMMAND vanta_permissions_transport_test
    COMMENT "Running PermissionsProtocol response transport checks")
endif()
add_dependencies(vanta_jni_shim vanta_permissions_transport_test)

install(TARGETS vanta_jni_shim jnivm logger ARCHIVE DESTINATION lib)

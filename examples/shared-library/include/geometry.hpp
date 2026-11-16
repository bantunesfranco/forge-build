#pragma once

// forge defines GEOMETRY_SHARED for the library and for everything that uses
// it when `geometry` is a shared library, and GEOMETRY_BUILDING only while the
// library's own sources compile. For a static library neither is defined, so
// this header works for both kinds.
#if defined(GEOMETRY_SHARED) && defined(_WIN32)
#  if defined(GEOMETRY_BUILDING)
#    define GEOMETRY_API __declspec(dllexport)
#  else
#    define GEOMETRY_API __declspec(dllimport)
#  endif
#elif defined(GEOMETRY_SHARED)
#  define GEOMETRY_API __attribute__((visibility("default")))
#else
#  define GEOMETRY_API
#endif

namespace geometry {

GEOMETRY_API double area(double width, double height);
GEOMETRY_API double diagonal(double width, double height);

}  // namespace geometry

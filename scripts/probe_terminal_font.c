/* Inspect the bundled font in this process; never installs it for the user. */
#include <CoreFoundation/CoreFoundation.h>
#include <CoreText/CoreText.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: probe_terminal_font /absolute/path/to/font.ttf\n");
        return 2;
    }
    CFURLRef url = CFURLCreateFromFileSystemRepresentation(NULL,
        (const UInt8 *)argv[1], (CFIndex)strlen(argv[1]), false);
    if (!url) return 2;
    CFErrorRef error = NULL;
    if (!CTFontManagerRegisterFontsForURL(url, kCTFontManagerScopeProcess, &error)) {
        if (error) CFRelease(error);
        CFRelease(url);
        fprintf(stderr, "font registration failed\n");
        return 1;
    }
    CTFontRef font = CTFontCreateWithName(CFSTR("JetBrainsMono Nerd Font"), 13.0, NULL);
    if (!font) { CFRelease(url); return 1; }
    CFStringRef family = CTFontCopyFamilyName(font);
    char name[256] = {0};
    if (!family || !CFStringGetCString(family, name, sizeof(name), kCFStringEncodingUTF8)) {
        if (family) CFRelease(family);
        CFRelease(font); CFRelease(url); return 1;
    }
    const UniChar chars[] = {0x276f, 0x2717, 0xe0b0, 0xf115};
    CGGlyph glyphs[sizeof(chars)/sizeof(chars[0])] = {0};
    bool all = CTFontGetGlyphsForCharacters(font, chars, glyphs, sizeof(chars)/sizeof(chars[0]));
    CGSize advances[sizeof(chars)/sizeof(chars[0])] = {{0}};
    CTFontGetAdvancesForGlyphs(font, kCTFontOrientationHorizontal, glyphs, advances,
                               sizeof(chars)/sizeof(chars[0]));
    printf("family=%s\n", name);
    for (size_t i = 0; i < sizeof(chars)/sizeof(chars[0]); i++) {
        printf("U+%04X glyph=%u advance=%.2f\n", chars[i], (unsigned)glyphs[i], advances[i].width);
        if (glyphs[i] == 0 || advances[i].width <= 0) all = false;
    }
    CFRelease(family); CFRelease(font); CFRelease(url);
    return !all || strcmp(name, "JetBrainsMono Nerd Font") != 0;
}

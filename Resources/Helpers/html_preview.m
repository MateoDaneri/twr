// Static document snapshot. Called only inside render_preview.py's Seatbelt job.
#import <Cocoa/Cocoa.h>
#import <WebKit/WebKit.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static const NSUInteger InputLimit = 2 * 1024 * 1024;
static const NSUInteger OutputLimit = 16 * 1024 * 1024;

static void fail(const char *message, int code) {
    // Never print document contents, paths, URL queries or framework diagnostics.
    fprintf(stderr, "%s\n", message);
    exit(code);
}

static BOOL canonical(NSString *path) {
    if (!path) return NO;
    char resolved[PATH_MAX];
    // Foundation maps /private/tmp back to /tmp for display. Use the same
    // filesystem identity as Python's resolve and the parent's Seatbelt paths.
    return realpath(path.fileSystemRepresentation, resolved)
        && strcmp(resolved, path.fileSystemRepresentation) == 0;
}

@interface Snapshot : NSObject <WKNavigationDelegate>
@property(strong) WKWebView *view;
@property(strong) NSString *output;
@end

@implementation Snapshot
- (void)webView:(WKWebView *)view
    decidePolicyForNavigationAction:(WKNavigationAction *)action
    decisionHandler:(void (^)(WKNavigationActionPolicy))decision {
    (void)view;
    // loadHTMLString uses about:blank. Meta refreshes and all other navigation
    // are refused, independently of the resource blocker and CSP.
    decision([action.request.URL.absoluteString isEqualToString:@"about:blank"]
        ? WKNavigationActionPolicyAllow : WKNavigationActionPolicyCancel);
}
- (void)webView:(WKWebView *)view didFinishNavigation:(WKNavigation *)navigation {
    (void)navigation;
    WKSnapshotConfiguration *configuration = [WKSnapshotConfiguration new];
    configuration.rect = CGRectMake(0, 0, 1500, 2200);
    configuration.snapshotWidth = @1500;
    [view takeSnapshotWithConfiguration:configuration completionHandler:^(NSImage *image, NSError *error) {
        if (error || !image) fail("HTML: snapshot failed", 3);
        CGImageRef raster = [image CGImageForProposedRect:NULL context:nil hints:nil];
        if (!raster || CGImageGetWidth(raster) > 3000 || CGImageGetHeight(raster) > 4400)
            fail("HTML: raster outside limits", 3);
        NSBitmapImageRep *bitmap = [[NSBitmapImageRep alloc] initWithCGImage:raster];
        NSData *png = [bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
        if (!png.length || png.length > OutputLimit) fail("HTML: PNG outside limits", 4);
        // The native probe failed an atomic Cocoa write and passed a direct
        // write. This private job is not published until successful exit.
        int fd = open(self.output.fileSystemRepresentation,
            O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, 0600);
        if (fd < 0) fail("HTML: cannot create private PNG", 4);
        const unsigned char *bytes = png.bytes;
        NSUInteger remaining = png.length;
        while (remaining) {
            ssize_t count = write(fd, bytes, remaining);
            if (count < 0 && errno == EINTR) continue;
            if (count <= 0) { close(fd); fail("HTML: PNG write failed", 4); }
            bytes += count;
            remaining -= (NSUInteger)count;
        }
        if (close(fd) != 0) fail("HTML: PNG close failed", 4);
        exit(0);
    }];
}
- (void)webView:(WKWebView *)view didFailProvisionalNavigation:(WKNavigation *)navigation
    withError:(NSError *)error {
    (void)view; (void)navigation; (void)error;
    fail("HTML: navigation failed", 5);
}
- (void)webView:(WKWebView *)view didFailNavigation:(WKNavigation *)navigation
    withError:(NSError *)error {
    (void)view; (void)navigation; (void)error;
    fail("HTML: document load failed", 5);
}
- (void)webViewWebContentProcessDidTerminate:(WKWebView *)view {
    (void)view;
    fail("HTML: content process stopped", 5);
}
@end

int main(int argc, const char **argv) {
    @autoreleasepool {
        if (argc != 3) fail("HTML: expected private document and output", 2);
        umask(0077);
        NSString *input = [NSString stringWithUTF8String:argv[1]];
        NSString *output = [NSString stringWithUTF8String:argv[2]];
        NSString *directory = output.stringByDeletingLastPathComponent;
        if (!input || !output || !input.isAbsolutePath || !output.isAbsolutePath
            || !canonical(input) || !canonical(directory)
            || ![input.stringByDeletingLastPathComponent isEqualToString:directory]
            || ![input.lastPathComponent isEqualToString:@"document.html"]
            || ![output.lastPathComponent isEqualToString:@"preview.png"])
            fail("HTML: invalid private paths", 2);
        struct stat folder, metadata;
        if (lstat(directory.fileSystemRepresentation, &folder) != 0
            || !S_ISDIR(folder.st_mode) || folder.st_uid != getuid() || (folder.st_mode & 0077))
            fail("HTML: output directory is not private", 2);
        int fd = open(input.fileSystemRepresentation, O_RDONLY | O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC);
        if (fd < 0 || fstat(fd, &metadata) != 0 || !S_ISREG(metadata.st_mode)
            || metadata.st_uid != getuid() || metadata.st_nlink != 1
            || (metadata.st_mode & 0077) || metadata.st_size < 0
            || metadata.st_size > (off_t)InputLimit)
            fail("HTML: invalid private document", 2);
        NSMutableData *data = [NSMutableData data];
        unsigned char buffer[65536];
        for (;;) {
            ssize_t count = read(fd, buffer, sizeof(buffer));
            if (count < 0 && errno == EINTR) continue;
            if (count < 0) fail("HTML: document read failed", 2);
            if (!count) break;
            if (data.length + (NSUInteger)count > InputLimit) fail("HTML: document too large", 2);
            [data appendBytes:buffer length:(NSUInteger)count];
        }
        close(fd);
        NSString *document = [[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];
        if (!document) fail("HTML: expected UTF-8 document", 2);

        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyProhibited];
        __attribute__((objc_precise_lifetime)) Snapshot *snapshot = [Snapshot new];
        snapshot.output = output;
        WKWebViewConfiguration *configuration = [WKWebViewConfiguration new];
        configuration.websiteDataStore = [WKWebsiteDataStore nonPersistentDataStore];
        configuration.defaultWebpagePreferences.allowsContentJavaScript = NO;
        configuration.preferences.javaScriptCanOpenWindowsAutomatically = NO;
        __attribute__((objc_precise_lifetime)) WKContentRuleListStore *store = [WKContentRuleListStore storeWithURL:
            [NSURL fileURLWithPath:directory isDirectory:YES]];
        NSString *rules = @"[{\"trigger\":{\"url-filter\":\".*\"},\"action\":{\"type\":\"block\"}}]";
        [store compileContentRuleListForIdentifier:@"static-private-preview" encodedContentRuleList:rules
            completionHandler:^(WKContentRuleList *list, NSError *error) {
                if (error || !list) {
                    NSString *detail = error.userInfo[NSHelpAnchorErrorKey]
                        ?: error.localizedDescription ?: @"No compiled rule list";
                    fprintf(stderr, "HTML: resource rule error %ld: %.1000s\n", (long)error.code, detail.UTF8String);
                    fail("HTML: resource rules failed", 6);
                }
                [configuration.userContentController addContentRuleList:list];
                snapshot.view = [[WKWebView alloc] initWithFrame:CGRectMake(0, 0, 1500, 2200)
                    configuration:configuration];
                snapshot.view.navigationDelegate = snapshot;
                NSString *prefix = @"<!doctype html><meta charset='utf-8'><meta http-equiv='x-dns-prefetch-control' content='off'><meta http-equiv='Content-Security-Policy' content=\"default-src 'none'; base-uri 'none'; form-action 'none'; script-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; media-src 'none'; img-src data:; font-src data:; style-src 'unsafe-inline'\">";
                [snapshot.view loadHTMLString:[prefix stringByAppendingString:document]
                    baseURL:[NSURL URLWithString:@"about:blank"]];
            }];
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 20 * NSEC_PER_SEC),
            dispatch_get_main_queue(), ^{ fail("HTML: snapshot timeout", 7); });
        [NSApp run];
        return 0;
    }
}

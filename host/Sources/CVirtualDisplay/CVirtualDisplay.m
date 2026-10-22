#import <Foundation/Foundation.h>
#import <CoreGraphics/CoreGraphics.h>
#import "CVirtualDisplay.h"

// Private CoreGraphics interfaces (not in the SDK). Declared here so the compiler knows the
// selectors; the classes themselves are resolved at runtime via NSClassFromString, so no private
// linker symbol is referenced. This is the long-standing CGVirtualDisplay interface used by
// headless-display tools (e.g. BetterDummy) and is stable across recent macOS releases.
@interface CGVirtualDisplayMode : NSObject
- (instancetype)initWithWidth:(uint32_t)width height:(uint32_t)height refreshRate:(double)refreshRate;
@end

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(nonatomic) uint32_t vendorID;
@property(nonatomic) uint32_t productID;
@property(nonatomic) uint32_t serialNum;
@property(copy, nonatomic) NSString *name;
@property(nonatomic) CGSize sizeInMillimeters;
@property(nonatomic) uint32_t maxPixelsWide;
@property(nonatomic) uint32_t maxPixelsHigh;
@property(nonatomic) CGPoint redPrimary;
@property(nonatomic) CGPoint greenPrimary;
@property(nonatomic) CGPoint bluePrimary;
@property(nonatomic) CGPoint whitePoint;
@property(copy, nonatomic) void (^terminationHandler)(id, id);
@end

@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray *modes;
@property(nonatomic) uint32_t hiDPI;
@end

@interface CGVirtualDisplay : NSObject
- (instancetype)initWithDescriptor:(id)descriptor;
- (BOOL)applySettings:(id)settings;
@property(readonly, nonatomic) uint32_t displayID;
@end

// Strong reference that keeps the virtual display alive for the process lifetime.
static CGVirtualDisplay *gVirtualDisplay = nil;

uint32_t aktive_virtual_display_create(uint32_t width, uint32_t height, double refresh_hz) {
    @autoreleasepool {
        Class descClass = NSClassFromString(@"CGVirtualDisplayDescriptor");
        Class modeClass = NSClassFromString(@"CGVirtualDisplayMode");
        Class settingsClass = NSClassFromString(@"CGVirtualDisplaySettings");
        Class displayClass = NSClassFromString(@"CGVirtualDisplay");
        if (!descClass || !modeClass || !settingsClass || !displayClass) {
            return 0; // API not present on this macOS; caller falls back to synthetic frames.
        }

        // A missing property selector on a future macOS would raise rather than silently misbehave;
        // catch it so the agent keeps running (and streams synthetic frames) instead of crash-looping.
        @try {
            CGVirtualDisplayDescriptor *descriptor = [[descClass alloc] init];
            descriptor.queue = dispatch_queue_create("com.aktivematrix.aktivestream.virtualdisplay", DISPATCH_QUEUE_SERIAL);
            descriptor.name = @"AktiveStream Display";
            descriptor.vendorID = 0x414B;  // "AK"
            descriptor.productID = 0x5653; // "VS"
            descriptor.serialNum = 0x0001;
            descriptor.maxPixelsWide = width;
            descriptor.maxPixelsHigh = height;
            // ~100 ppi physical size (only affects reported DPI, not captured pixels).
            descriptor.sizeInMillimeters = CGSizeMake((double)width * 25.4 / 100.0, (double)height * 25.4 / 100.0);
            // Standard sRGB / Rec.709 primaries and D65 white point.
            descriptor.redPrimary = CGPointMake(0.640, 0.330);
            descriptor.greenPrimary = CGPointMake(0.300, 0.600);
            descriptor.bluePrimary = CGPointMake(0.150, 0.060);
            descriptor.whitePoint = CGPointMake(0.3127, 0.3290);
            descriptor.terminationHandler = ^(id a, id b) { (void)a; (void)b; };

            CGVirtualDisplay *display = [[displayClass alloc] initWithDescriptor:descriptor];
            if (!display) {
                return 0;
            }

            id mode = [[modeClass alloc] initWithWidth:width height:height refreshRate:refresh_hz];
            CGVirtualDisplaySettings *settings = [[settingsClass alloc] init];
            settings.hiDPI = 0; // 1:1 pixels, so capture matches the 1920x1200 stream size.
            settings.modes = @[mode];

            if (![display applySettings:settings]) {
                return 0;
            }

            gVirtualDisplay = display; // retain for the process lifetime
            return display.displayID;
        } @catch (NSException *exception) {
            return 0;
        }
    }
}
